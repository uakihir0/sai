# 04. Modules and Names

**Status:** Normative
**Stage:** 0 (module tree, `use` within the current package and of `sai:core`/`sai:alloc`, visibility, name resolution); 3 (dependencies on other packages, user effects); 4 (WIT interfaces, worlds)
**Related:** [01 Introduction](01-introduction.md), [02 Lexical Structure](02-lexical-structure.md), [03 Grammar](03-grammar.md), [05 Types](05-types.md), [06 Expressions and Statements](06-expressions-and-statements.md), [08 Effects and Capabilities](08-effects-and-capabilities.md), [13 Components and WIT](13-components-and-wit.md), [15 Attributes and Lints](15-attributes-and-lints.md), [16 Core Library](16-core-library.md), [package/01 Package Contract](../package/01-package-contract.md), [package/02 Resolver](../package/02-resolver.md), [impl/03 Resolver and Type Checker](../impl/03-name-resolution-and-type-checking.md), [impl/12 Semantic Tooling](../impl/12-semantic-tooling.md), [decisions](../design/decisions.md)

This chapter defines how a package's source files form a tree of modules, how `use` declarations bind names, which names are visible where, and how every identifier and path in a program is resolved to the entity it denotes. It also defines the canonical path of every nameable entity, which [impl/12](../impl/12-semantic-tooling.md) uses for symbol IDs. Type-relative names (methods, associated functions, associated types, fields) are resolved by [05](05-types.md) and [06](06-expressions-and-statements.md). This chapter only states where that hand-off happens. The syntax of every construct mentioned here is in [03](03-grammar.md).

## 4.1 Conventions

### 4.1.1 Terms

- **Package.** The unit of distribution and compilation, identified by a **package identity** `ns:name` (D-PKG-01; [package/01](../package/01-package-contract.md)).
- **Module.** One source file. A module is identified within its package by a **module path**: a possibly empty sequence of segments. The root module has the empty module path.
- **Item.** A top-level declaration produced by the `ItemBody` production of 03 §3.2: a function, type (record, alias or refined type), enum, distinct type, resource, trait, impl, constant, invariant, effect, interface, world, extern block, test, property, or `use` declaration.
- **Entity.** Anything a name can denote: a package, a module, an item, a member of an item (variant, field, associated function or method, associated type, interface member), a generic parameter, or a local binding.
- **Binding.** An association of a name with an entity in some scope.
- **Scope.** A region of source text in which a set of bindings is visible. Scopes nest (§4.7).
- **Module scope.** The set of bindings visible at the top level of a module (§4.4.1).
- **Member namespace.** The set of names an entity makes available after a `.` (§4.4.2).
- **Canonical path.** The unique textual identity of an entity (§4.9).

### 4.1.2 Notation

- `pkg(M)` is the package containing module `M`. `path(M)` is its module path, written with `/` between segments.
- `D(M)` is the set of item bindings declared in module `M`. `U(M)` is the set of bindings introduced by its `use` declarations. `X(M)` is its member namespace (the bindings other modules can reach through `M`).
- `M ⊢ p ↦ e` means that in module `M` the path `p` resolves to entity `e`.
- `vis(e)` is the visibility of entity `e` (§4.6). `visible(e, M)` holds when `e` may be named from module `M`.

### 4.1.3 Example conventions

Examples show the file they belong to in a leading comment. Unless stated otherwise, the current package is `acme:app`.

## 4.2 Packages and the module tree

### 4.2.1 Package identity

The identity of the current package is the `package` field of its `sai.package` ([package/01](../package/01-package-contract.md)). A toolchain that implements only Stages 0–2 MUST still read this field to learn the package identity, and MAY ignore every other field (N-A-1). The identity has the form `PkgSeg ":" PkgSeg` of 02 §2.9. Its two parts are the **namespace** and the **package name**.

Packages other than the current package, `sai:core` and `sai:alloc` (§4.2.6) are available only from Stage 3. Before Stage 3, a `use` that names any other package is the error `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md)), naming Stage 3.

### 4.2.2 Source directory

Every package with SAI source has one **source directory**, written `<source>`. It is the directory named by the `source` declaration of `sai.package`, relative to the package directory ([package/01](../package/01-package-contract.md) §1.6.2). There is no default: a package without `source` has no SAI source (package/01 N-G-6). The examples of this specification declare `source "src"`. Only `<source>` contributes modules. Files outside it (tests directories, build scripts, assets) are never modules.

### 4.2.3 From files to modules

The module tree is computed from the file system by the following deterministic procedure (D-SYN-08, R-10, N-A-2). The procedure depends only on the names and kinds of directory entries, never on their timestamps, permissions or enumeration order.

1. If `<source>` does not exist or is not a directory, the package has no root module: error `MISSING_ROOT_MODULE`.
2. Walk `<source>` recursively. In each directory, process the entries in ascending byte order of their names.
3. Ignore every entry whose name begins with `.`, together with its contents.
4. A symbolic link (to a file or a directory) is the error `SYMLINK_IN_SOURCE`. It contributes nothing to the tree. Source layout must not depend on links that differ between checkouts (P6).
5. Ignore every regular file whose name does not end in `.sai`. Such files are not modules and produce no diagnostic.
6. A file `<source>/lib.sai` is the **root module**, with the empty module path.
7. Any other file `<source>/s1/…/sk/f.sai` (k ≥ 0) is the module with path `s1/…/sk/f`.
8. Every segment `s1 … sk` and the file stem `f` MUST be a valid **module segment**:
   - it consists only of ASCII characters; otherwise the error is `NON_ASCII_PUBLIC_NAME` (02 §2.4);
   - it matches `[a-z][a-z0-9_]*`;
   - it is not a strict keyword or reserved word (02 §2.5.1, §2.5.3). Contextual keywords are allowed;
   - it is not `lib`, except as the stem of the root file `<source>/lib.sai`. A directory `<source>/lib/` or a file `<source>/a/lib.sai` is invalid (D-SYN-08).

   A segment that violates the last three rules is the error `INVALID_MODULE_NAME`. A file with an invalid path segment contributes no module, and neither does anything below an invalid directory.
9. If `<source>/lib.sai` does not exist, the error is `MISSING_ROOT_MODULE`.
10. A directory `<source>/…/a/` that has no sibling file `a.sai` is a pure **namespace**. It contributes no module of its own; the module path `…/a` does not exist. A directory and a file with the same stem coexist: `a.sai` is module `a` and `a/b.sai` is module `a/b`.
11. Empty directories are ignored.

There is no other way to create a module. There are no `mod` declarations, no inline modules, and no conditional inclusion of files. Conditional compilation is per item (`@target`, [15](15-attributes-and-lints.md)).

Because every valid segment is lowercase ASCII, two valid paths can never differ only in letter case. A package therefore builds identically on case-sensitive and case-insensitive file systems.

```text
acme-app/
  sai.package          declares `package acme:app @ …` and `source "src"`
  src/
    lib.sai            module acme:app            (root)
    model.sai          module acme:app/model
    model/
      user.sai         module acme:app/model/user
    http/              namespace only (no http.sai)
      client.sai       module acme:app/http/client
    notes.txt          ignored
    .scratch.sai       ignored (leading dot)
```

### 4.2.4 The module header

A module may start with inner documentation comments and an `unsafe module` header (03 §3.2, D-SYN-08). The header does not affect naming. Its meaning is defined in [12](12-unsafe-and-ffi.md).

### 4.2.5 Parent and child modules

Module paths form a tree only for naming and display. A module has no special relationship to the modules below it or above it:

- A submodule is **not** a member of its parent module. `acme:app/model/user` is not reachable as `model.user` through a `use` of `acme:app/model`; it must be imported by its own `use` (N-A-3).
- A child module sees exactly the same items of its parent as any other module of the package. A parent sees no more of its children than any other module.
- Visibility has no module level (D-MOD-02). Every module of a package can name every package-private item of every other module of the same package.

Rationale: a `use` names exactly one module (P1), and moving a file never silently changes what another module's paths resolve to (P8).

### 4.2.6 Implicit packages and the prelude

- `sai:core` and `sai:alloc` are implicit dependencies of every package. They are never declared in `sai.package` (D-PKG-04); declaring them is an error defined in [package/01](../package/01-package-contract.md). Both can be named in `use` declarations at every stage: `use sai:core/fmt.{Format}`.
- Their module trees and item lists are defined by [16](16-core-library.md).
- Names from `sai:alloc` can always be *resolved*. Whether a program may *use* them in a given target and profile is decided by the `alloc` effect ([08](08-effects-and-capabilities.md)) and by [targets](../targets/targets-and-profiles.md), never by name resolution.
- Every other standard package (`sai:collections`, `sai:http`, `sai:fs`, …) is an ordinary dependency that must be declared (D-PKG-04).
- The **prelude** is a fixed set of bindings implicitly visible in every module, after all other scopes (§4.5.1). Its contents are defined by [16](16-core-library.md). It includes at least the primitive type names of D-TYPE-01, `Never`, `Option`, `Some`, `None`, `Result`, `Ok`, `Err`, `String`, `Vec`, `Box`, `Bytes`, `Str`, `Slice`, `SliceMut`, `Array`, and the traits listed in D-TYPE-08.
- For name resolution, `sai:core` and `sai:alloc` are two separate packages with separate module trees. For coherence they count as one package ([05](05-types.md), D-TYPE-08).

## 4.3 `use` declarations

### 4.3.1 Forms

The syntax is `UseDecl` of 03 §3.4. Every form (D-MOD-01, D-MOD-04):

```sai
// src/lib.sai of acme:app

use acme:app/model/user // a module of the current package: full identity
use acme:user-client // root module of a dependency, bound as `user_client`
use sai:core/fmt // binds the module as `fmt`
use sai:core/fmt as format // binds the module as `format`
use sai:core/fmt as format.{Format} // binds `format` and `Format`
use sai:core/fmt.{Format, Writer as Out} // binds the items `Format` and `Out` only

pub use acme:app/model/user.{User, UserId} // re-exports two items from this module
```

The block is in canonical order: the formatter sorts `use` declarations and puts the `pub use` group last ([impl/14](../impl/14-formatter.md) §14.7.2). Its repeated bindings of `format` and `Format` are `REDUNDANT_IMPORT` (§4.3.6); they are shown together only to list the forms.

- There are no glob imports and no relative paths (D-MOD-01). Modules of the current package are named with the current package's identity.
- `use` declarations are items. They may appear anywhere among a module's items, and their order has no meaning. The formatter moves them to the top ([impl/14](../impl/14-formatter.md)).
- A `use` declaration is never inside a block (D-SYN-10).

### 4.3.2 Resolving a `PKG_PATH`

A `PKG_PATH` `ns:pkg/s1/…/sn` (n ≥ 0) in a `use` declaration of module `M` resolves to a **use target** as follows (N-A-4).

1. **Package.** Let `K` be `ns:pkg`.
   1. If `K` is the identity of the current package, the target package is the current package.
   2. Otherwise, if `K` is `sai:core` or `sai:alloc`, the target package is that package. If `K` is one of the WASI packages shipped with the toolchain ([13](13-components-and-wit.md) §13.7.1 item 1), the target package is that WIT package (Stage 4).
   3. Otherwise, if `K` is a dependency declared in `sai.package` ([package/01](../package/01-package-contract.md)), the target package is the package selected for it by [package/02](../package/02-resolver.md). Before Stage 3 this step is `FEATURE_NOT_IMPLEMENTED`.
      - If the requirement has an `only` list that does not contain the target of the build ([package/01](../package/01-package-contract.md) §1.10.5), the dependency is not active, and the error is `INACTIVE_DEPENDENCY`, reported at the `PKG_PATH`. The help names the targets of the `only` list and suggests making the `use` and the items that need it target-specific with `@target` ([15](15-attributes-and-lints.md) §15.11). A `use` removed by `@target` is never checked, because it does not exist for the build (§15.11.3 of 15).
      - A requirement of `requires test` ([package/01](../package/01-package-contract.md) §1.10.6) is a declared dependency only in the code in which [14](14-testing.md) makes test dependencies available. The error for other code is defined there.
   4. Otherwise the error is `UNKNOWN_PACKAGE`. The diagnostic lists declared dependencies whose identity is within edit distance 2 of `K`.
2. **SAI package.** If the target package is a SAI package:
   1. If the target has a module with path `s1/…/sn`, the use target is that module. For n = 0 this is the root module.
   2. Otherwise, if n ≥ 1 and the target has a module `P` with path `s1/…/s(n-1)` (the root module when n = 1), and `P` declares an interface named `sn`, the use target is that interface (Stage 4). If `P` is in another package, the interface must also be visible from `M` (§4.6), otherwise the error is `PRIVATE_ITEM`.
   3. Otherwise the error is `UNKNOWN_MODULE`. It names the longest prefix that resolves to an existing module or namespace directory and lists the modules directly below it. If a segment is `lib`, the diagnostic adds the help "the root module is named by the package identity alone".
   4. If the use target is `M` itself, the error is `USE_OF_CURRENT_MODULE` (N-A-19). The items of `M` are already in scope.

   Segments are matched exactly against module and interface names. The kebab-to-snake mapping does not apply to segments of SAI packages. A segment containing `-` therefore never matches a SAI module or interface, and the diagnostic suggests the snake form (N-A-4).
3. **WIT package (Stage 4).** If the target package is a WIT package ([13](13-components-and-wit.md)), the segments are matched against the WIT package and the use target is an interface of its SAI view ([13](13-components-and-wit.md) §13.7.3). A WASI package is a WIT package. So is an artifact or hybrid dependency, which its dependents see only through its `wit` directory ([package/04](../package/04-distribution-and-security.md) §4.5.3 item 1). Other source dependencies are SAI packages (step 2).
   1. n MUST be 1, and `s1` MUST be the WIT name of an interface of that package, written in lowercase ([13](13-components-and-wit.md) §13.7.2). The use target is that interface.
   2. n = 0 (a whole package) or a name that denotes a world is the error `INVALID_USE_TARGET`. For a name that matches nothing, the error is `UNKNOWN_WIT_ITEM` ([13](13-components-and-wit.md) §13.7.2).
   3. n ≥ 2 is the error `UNKNOWN_MODULE`.

   Versions are never written in a `PKG_PATH`. They come from `sai.package` and `sai.lock`.

In the package that defines them, a module `P` that declares an interface `b` and a module file `P/b.sai` (for the root, a file `<source>/b.sai`) MUST NOT both exist. Their coexistence would make the path `…/b` ambiguous, so it is the error `MODULE_INTERFACE_CONFLICT`, reported on the interface declaration (N-A-4).

### 4.3.3 The binding name of a use target

A `use` declaration without `UseItems`, or with both `UseAlias` and `UseItems`, binds the use target under one **binding name** (N-A-5):

1. If `as a` is given, the binding name is `a`.
2. Otherwise, if n ≥ 1, it is the last segment `sn`, with every `-` replaced by `_`.
3. Otherwise (n = 0, a package root), it is the package name, the part after `:`, with every `-` replaced by `_` (D-MOD-01: `use acme:user-client` binds `user_client`).

The binding name MUST be a `LIDENT` (03 §3.4 requires that of an alias, and a derived name is always lowercase). If the derived name is a strict keyword or a reserved word, the error is `USE_BINDING_IS_KEYWORD` and an alias is required: `use acme:type` is rejected, and `use acme:type as type_pkg` is accepted. A derived name that equals a contextual keyword or a built-in effect root (`fs`, `net`, …) is allowed. A binding named like a built-in effect root cannot be named in effect position ([08](08-effects-and-capabilities.md) §8.2.2).

A module or interface binding introduced this way is a **module alias**. Below, "module alias" covers both module and interface use targets.

### 4.3.4 Item lists

`UseItems` imports individual members of the use target (D-MOD-01, R-01).

- A `use` with `UseItems` and no `UseAlias` binds **only** the listed items. The module itself is not bound (N-A-6). To bind both, write an alias, which may be the default name: `use sai:core/fmt as fmt.{Format}`.
- Each `UseItem` `x` or `x as y` names a binding `x` in the member namespace `X(T)` of the use target `T` (§4.4.2).
  - If `X(T)` has no binding `x`, the error is `UNRESOLVED_IMPORT`. The diagnostic lists up to three names of `X(T)` within edit distance 2.
  - If the binding exists but its entity is not visible from the importing module (§4.6), the error is `PRIVATE_ITEM`.
  - Otherwise the importing module gets a binding `y` (or `x`) for the same entity.
- `y` MUST have the same case class as `x` (03 §3.4). A mismatch is the error `ALIAS_CASE_MISMATCH`.
- Only module-level items can be imported: functions, types, enums, distinct types, resources, traits, constants, effects, interfaces and worlds (including items reached through `pub use`). Enum variants, fields, methods, associated functions and associated types are members of a type or trait, not of a module, so they cannot be listed. Naming one is `UNRESOLVED_IMPORT`, with the help "import the type and write `Type.Member`" (N-A-20). The prelude variants `Some`, `None`, `Ok` and `Err` are prelude bindings, not imports (§4.2.6).
- The members of an interface use target are its types, resources and functions (§4.8.1).
- The same name bound twice by one list, or by a list and another declaration, follows the duplicate rules of §4.4.3.

```sai
// src/lib.sai of acme:app
use acme:app/model.{User, Shape}
use acme:app/model.{Circle}              // rejected: UNRESOLVED_IMPORT (variants are not module members; use `Shape.Circle`)
use acme:app/model.{User as user}        // rejected: ALIAS_CASE_MISMATCH
```

### 4.3.5 Re-exports with `pub use`

`pub use` adds bindings to the member namespace of the module that contains it (D-MOD-04).

- A `pub use` MUST have `UseItems` (03 §3.4). Without them the error is `PUB_USE_WITHOUT_ITEMS`. Modules cannot be re-exported.
- Each listed item MUST be `pub` (§4.6). Re-exporting a package-private item would publish it, so it is the error `PRIVATE_REEXPORT`. A `pub use` never creates package-private aliases; within a package, items are reached through their defining module.
- A `pub use` binding is visible wherever the containing module is reachable, like a `pub` item of that module.
- A re-exported item **keeps the identity of its definition**: the same canonical path (§4.9), the same symbol ID, the same type, trait and impls. `acme:app.User` re-exported from `acme:app/model/user` *is* `acme:app/model/user.User`. Re-exporting never creates a new type (D-MOD-04).
- An item may be re-exported from the current package or from a dependency. Rules about dependencies that appear in a package's public API are in [package/02](../package/02-resolver.md).
- A re-export of a re-export is allowed. Resolution follows the chain to the definition (§4.5.4). A chain that returns to a binding already on it, without reaching a definition, is the error `REEXPORT_CYCLE` (D-MOD-04).
- `pub use` with an alias (`pub use sai:core/fmt as fmt.{Format}`) is allowed. The items are re-exported. The alias `fmt` is an ordinary module-local binding and is not re-exported.
- A `pub use` binding is never reported as unused.

```sai
// src/lib.sai of acme:app: a facade over the internal module tree
pub use acme:app/http/client.{fetch as fetch_user}
pub use acme:app/model/user.{User, UserId}
```

A dependent package imports the re-exported items from the root module of `acme:app`:

```sai
use acme:app.{User, fetch_user}
```

### 4.3.6 Unused and redundant imports

- A binding introduced by a non-`pub` `use` that is never referenced in its module is the warning `UNUSED_IMPORT` ([15](15-attributes-and-lints.md)). References are counted after `@target` filtering. A reference from any item counts, including a reference from a test or property. With test analysis (`sai test`, `sai check --tests`) these are the references that the analysis finds; without it, tests and properties are not resolved, and a binding counts as referenced if its name occurs as an identifier token in a test or property of the same module ([14](14-testing.md) §14.2.4 item 4, N-I-39). For a `use` with both an alias and items, each binding is checked separately.
- A `use` binding that denotes an entity the same name already denotes in the module scope or in the prelude is the warning `REDUNDANT_IMPORT`. Examples: importing `Option` from `sai:core/option` under its prelude name, and importing the same item twice under the same name. Two bindings of the *same name* to *different* entities are never redundant; they are `DUPLICATE_BINDING` (§4.4.3) or shadow the prelude (§4.4.4).

## 4.4 Namespaces

### 4.4.1 The module scope

Each module has **one namespace** that holds types, values, modules, traits, effects, interfaces and worlds alike (D-SYN-03). The module scope `S(M)` is `D(M) ∪ U(M)`:

| Source of binding | Name | Entity |
|---|---|---|
| `fn f` (including `export fn`, `unsafe fn`, `async fn`) | `f` | function item |
| `type T {..}`, `type T = ..`, `type T = .. where ..` | `T` | record, alias, refined type |
| `enum E`, `distinct D`, `resource R` | `E`, `D`, `R` | nominal type |
| `trait Tr` | `Tr` | trait |
| `const C` | `C` | constant |
| `effect e` | `e` | user effect (Stage 3) |
| `interface i` | `i` | interface (Stage 4) |
| `world w` | `w` | world (Stage 4) |
| a function inside `extern … { }` | its name | extern function (Stage 7 for C; `"wasm"` per [12](12-unsafe-and-ffi.md)) |
| `use P` / `use P as a` | binding name (§4.3.3) | module or interface (module alias) |
| `use P.{x as y}` | `y` | the imported entity |
| `pub use P.{x as y}` | `y` | the re-exported entity |

These items introduce **no** binding: `impl` blocks, `invariant` declarations (which name an existing type, 03 §3.8), `test` and `property` declarations (named by a string), and the extern block itself.

Names in the module scope are independent of order. An item may refer to an item declared later in the same file, and to any item of any module of the package (D-SYN-03).

### 4.4.2 Member namespaces

After a `.`, a name is looked up in the **member namespace** of the entity before the dot:

| Entity | Member namespace |
|---|---|
| module `M` | `X(M)`: every item declared in `M` (with its visibility) and every `pub use` binding of `M`. Plain `use` bindings of `M` are **not** members; they are private to `M` (N-A-7). |
| interface | its types, resources and functions (§4.8.1) |
| enum | its variants (upper) and its associated functions (lower) |
| record, resource, distinct, refined type, type alias | its associated functions (lower) |
| trait | its methods (lower) and associated types (upper) |
| type parameter, `Self`, associated type | its associated types (upper, in type position) and the methods of its bounds (lower) |
| world | none; worlds are referenced only as a whole (§4.8.2) |

Fields are not in any member namespace. `r.f` without call parentheses is a field access ([06](06-expressions-and-statements.md) §6.6), and `r.f(..)` is always a method call. A type may therefore have a field and a method with the same name.

Type-relative members (variants, associated functions, methods, associated types) are resolved by [05](05-types.md), because the set of members depends on impls. This chapter resolves only the part of a path up to and including the first segment that denotes a type, trait or type parameter (§4.5.2).

### 4.4.3 Duplicate names

- Two bindings of the same name in one module scope are the error `DUPLICATE_BINDING`, reported on the later one in source order, with a note on the earlier one (D-SYN-03). This applies to every pair from §4.4.1: two declarations, a declaration and an import, or two imports of different entities. The code is the same one D-MOD-03 uses for a block, because the rule is the same: within one scope a name means one thing (N-A-8).
- Two imports of the same name that denote the **same** entity are the warning `REDUNDANT_IMPORT` (§4.3.6), not an error.
- Case keeps the most common kinds apart without any rule: `type Point` and `fn point` can coexist, because `Point` and `point` are different names.
- `@target`-filtered items take part only if their predicate holds ([15](15-attributes-and-lints.md)). Two items with the same name are therefore allowed when at most one of them survives for every target.

```sai
// src/model.sai
type Point {
    x: I32
    y: I32
}
fn point(x: I32, y: I32) -> Point { Point(x, y) }      // OK: different name
const point: I32 = 0                                    // rejected: EXPECTED_TOKEN (03), a constant needs an upper name
fn point() { }                                          // rejected: DUPLICATE_BINDING
```

Each member namespace has its own duplicate rule:

| Duplicate | Code | Owner |
|---|---|---|
| two fields of one record, resource or record variant | `DUPLICATE_FIELD` | [05](05-types.md) |
| two variants of one enum | `DUPLICATE_VARIANT` | [05](05-types.md) |
| two items of the same name in the inherent impls of one type | `DUPLICATE_INHERENT_ITEM` | [05](05-types.md) |
| two items of one trait or trait impl | `DUPLICATE_TRAIT_ITEM` | [05](05-types.md) |
| two members of one interface | `DUPLICATE_BINDING` | this chapter |
| two parameters of one function or closure, or two generic parameters of one list | `DUPLICATE_BINDING` | this chapter |
| two bindings of one name in one pattern | `DUPLICATE_BINDING` | this chapter (03 §3.14 states the rule) |

### 4.4.4 Reserved names and the prelude

- The primitive type names `I8 I16 I32 I64 U8 U16 U32 U64 F32 F64 Bool Byte Char ISize USize`, the aliases `Int` and `UInt`, and `Never` are **reserved type names** (D-TYPE-01). They are prelude bindings that no module may rebind. Declaring an item with one of these names, or using one as the name or alias of an imported item, is the error `RESERVED_TYPE_NAME` (N-A-9). Rationale: `I32` must mean the same thing in every file (P8).
- The names `Send` and `Share` are also reserved, for future auto traits ([11](11-async-and-concurrency.md) §11.11, N-I-19). They are not prelude bindings, but declaring an item named `Send` or `Share`, or using one as the name or alias of an imported item, is likewise `RESERVED_TYPE_NAME` (N-A-9).
- Every other prelude name (`Option`, `Some`, `String`, `Vec`, `Clone`, …) may be rebound by a declaration or import in the module scope. The module binding then wins (§4.5.1), and the warning `SHADOWS_PRELUDE` is reported on the declaration or import (N-A-9). Rationale: adding a name to the prelude in a later version must not break existing programs (P8). The lint makes the shadowing visible (P1).
- `self`, `Self`, `true` and `false` are keywords, not names. They cannot be declared and are not looked up in scopes. `Self` is resolved as in §4.5.3.

### 4.4.5 Namespaces outside the module scope

The following names are not part of the module namespace or of any member namespace:

| Names | Namespace | Defined in |
|---|---|---|
| loop labels (`'outer`) | one per function or closure body | [06](06-expressions-and-statements.md) (`UNDECLARED_LABEL`, `LABEL_SHADOWED`) |
| built-in effect roots (`alloc`, `fs`, `net`, …) | effect position only; they take precedence over every binding | [08](08-effects-and-capabilities.md) §8.2.2 |
| attribute names | fixed set | [15](15-attributes-and-lints.md) |
| `@derive` arguments | the fixed derivable trait names; not resolved in scope | [15](15-attributes-and-lints.md), [05](05-types.md) |
| lint and diagnostic codes in `@allow`/`@warn`/`@deny` | one global namespace (D-TOOL-01) | [15](15-attributes-and-lints.md) |
| field names | per record type or record variant | [05](05-types.md) |
| record-construction and record-pattern field labels | the fields of the constructed type | [06](06-expressions-and-statements.md) |
| package identities | dependency set of `sai.package` | [package/01](../package/01-package-contract.md) |

## 4.5 Name resolution

### 4.5.1 Lookup order for an unqualified name

An unqualified name `x`, or the first segment of a path, is looked up in these scopes in order. The first scope that has a binding for `x` decides (N-A-10):

1. **Local bindings**, innermost scope first (§4.7): pattern bindings of `let`, `var`, `match` arms, `if`/`while` conditions and `for` loops; closure parameters; function parameters. `self` is a keyword handled in §4.5.3, not a local.
2. **Generic parameters**, innermost item first: the function's own generic parameters, then those of the enclosing `impl` or `trait`.
3. **The module scope** `S(M)` (§4.4.1). Declared items and `use` bindings form one namespace, so there is no order between them.
4. **The prelude** (§4.2.6).

If no scope has a binding, the error is `UNRESOLVED_NAME`. The diagnostic lists up to three bindings within edit distance 2 that are visible at the point of use. If a module of the current package or a declared dependency has a visible item with that exact name, it adds a fix that inserts the corresponding `use`.

The case rule (D-LEX-02) means locals and generic parameters never compete. Locals are always lowercase and generic parameters are always uppercase. Only the following lookups can actually shadow:

| Inner binding | Shadows | Allowed? |
|---|---|---|
| local | module-level function, extern function, imported value | yes, silently (D-MOD-03) |
| local | module alias (`use` of a module or interface) | no: `SHADOWS_MODULE` (D-MOD-01, §4.7.3) |
| generic parameter | module-level type, trait or constant of the same name | yes, silently |
| generic parameter | generic parameter of an enclosing item | no: `SHADOWS_GENERIC_PARAM` (§4.7.4) |
| module-level binding | prelude binding | yes, with `SHADOWS_PRELUDE`; not for reserved names (§4.4.4) |

**Position filters.** The lookup itself ignores kinds. Position restricts which scopes take part:

- In **type position**, step 1 is skipped (locals are not types).
- In **pattern position**, a bare lowercase identifier is never looked up. It always introduces a new binding (D-LEX-02). A bare uppercase identifier, or a path, is looked up by steps 2–4. If it resolves to a generic parameter, the error is `WRONG_NAME_KIND`.
- In **effect position**, the built-in effect roots are matched first, as [08](08-effects-and-capabilities.md) §8.2.2 defines. Otherwise lookup starts at step 3: locals and generic parameters are not effects.
- In a **`@derive`, lint or other attribute argument**, no lookup happens (§4.4.5).

After lookup, the entity's kind must fit the position. A type where a value is expected is `EXPECTED_VALUE_FOUND_TYPE` ([06](06-expressions-and-statements.md)). Every other mismatch is `WRONG_NAME_KIND`, which names the expected and the found kind: a module used as a value, a function in type position, a trait used as a type (there is no `dyn`, D-TYPE-02), a constant used as a type, an effect outside effect position, a world outside a `WorldRef`.

### 4.5.2 Qualified paths

A path `s1.s2.….sn` in expression, type, pattern or effect position is resolved segment by segment:

1. Resolve `s1` by §4.5.1.
2. While the current entity is a **module alias** (module or interface), resolve the next segment in its member namespace (§4.4.2). A missing member is `UNRESOLVED_NAME` ("no item `s` in module `acme:app/model`"). A member that exists but is not visible from the current module is `PRIVATE_ITEM`.
3. If the current entity is a **local binding** or a **constant**, the rest of the path is not a path. `x.f` is a field access or a method call on the value `x`, and `C.f` one on the value of the constant `C` ([06](06-expressions-and-statements.md) §6.4.1, [05](05-types.md)) (N-A-67).
4. If the current entity is a **type** (nominal type, alias, refined type, primitive), a **type parameter**, `Self` or a **trait**, the remaining segments are type-relative and are resolved by [05](05-types.md): variants, associated functions, associated types, and trait methods in path form (`Trait.m(x)`).
5. If the current entity is a function, effect or world and segments remain, the error is `WRONG_NAME_KIND`. The one exception is `w.f`, where `w` is a world of the current package and `f` is one of its imported functions ([13](13-components-and-wit.md) §13.4.1).

Generic arguments may follow any segment that denotes a generic entity (D-SYN-04). Their number and kinds are checked by [05](05-types.md).

```sai
// src/lib.sai
use acme:app/model
use acme:app/model/user.{User}

fn demo(u: User) -> model.Shape {
    let name = u.name // `u` is a local: field access (06)
    let s = model.Shape.Circle(1.0) // module alias, then type, then variant (05)
    let n = User.create(name) // type, then associated function (05)
    let x = model.ORIGIN.x // module alias, then constant, then field access (06)
    s
}
```

### 4.5.3 `self` and `Self`

- `self` is the receiver parameter of a method (`SelfParam`, 03 §3.5). It is in scope in the function body exactly like a parameter. It can never be shadowed: `self` is a keyword, so no pattern can bind it. Inside a refinement or invariant predicate, `self` denotes the value being checked ([09](09-contracts-and-refinements.md)). Anywhere else `self` is `SELF_OUTSIDE_IMPL` ([05](05-types.md)).
- `Self` in a trait or impl denotes the implementing type. `Self.X` in type position names an associated type, and `Self.f` in expression position names an associated function or method ([05](05-types.md)). Outside a trait or impl, `Self` is `SELF_OUTSIDE_IMPL` ([05](05-types.md)). An interface's resources use `Self` as defined in [13](13-components-and-wit.md).

### 4.5.4 The resolution algorithm and cycles

Name resolution for a package is the following procedure. It is deterministic, and its result does not depend on the order in which files are read (N-A-10).

**Phase 1: collect.** For every module `M` of the package, after removing items whose `@target` predicate is false ([15](15-attributes-and-lints.md)), build `D(M)` from the item declarations alone. Report `DUPLICATE_BINDING` and `RESERVED_TYPE_NAME` among declarations. This phase reads no `use` declaration.

**Phase 2: import tables.** For every `use` declaration of every module, resolve its `PKG_PATH` (§4.3.2). For a dependency, the dependency's member namespaces are already final, because dependencies are resolved first and package cycles are impossible ([package/02](../package/02-resolver.md)). For a module of the current package, compute the member namespace `X(M)` lazily, by the following function on pairs `(module, name)`:

```text
lookup(M, x):
    if (M, x) ∈ in_progress: error REEXPORT_CYCLE on every `pub use` on the cycle; return ⊥
    if x ∈ D(M): return the declared entity
    if M has `pub use T.{y as x}`:
        in_progress ∪= {(M, x)}
        e := lookup(T, y)                   // T resolved by §4.3.2; T may be in a dependency
        in_progress ∖= {(M, x)}
        if e = ⊥: error UNRESOLVED_IMPORT; return ⊥
        if not visible(e, M) or vis(e) ≠ pub: error PRIVATE_ITEM / PRIVATE_REEXPORT
        return e
    return none
```

Each pair is computed at most once and memoized, so the procedure terminates. A re-export cycle is reported once, with every `pub use` on the cycle as a note (D-MOD-04).

**Phase 3: module scopes.** For every module, `S(M) = D(M) ∪ U(M)`, where `U(M)` holds the binding names of §4.3.3 and the items of §4.3.4. Report `DUPLICATE_BINDING`, `REDUNDANT_IMPORT`, `SHADOWS_PRELUDE`, `ALIAS_CASE_MISMATCH` and `USE_BINDING_IS_KEYWORD`.

**Phase 4: resolve uses.** Resolve every path in every item signature, body, contract, attribute target and world, by §4.5.1–§4.5.3. Report `UNRESOLVED_NAME`, `PRIVATE_ITEM`, `WRONG_NAME_KIND` and `SHADOWS_MODULE`. Type-relative segments are recorded for [05](05-types.md). After this phase, report `UNUSED_IMPORT`.

Consequences:

- **Module cycles are allowed.** Module `a` may `use` module `b` while `b` uses `a`. Phase 1 needs no imports, and Phase 2 needs only the declared items of the modules involved.
- **Item-level cycles** are not name-resolution errors. Mutually recursive functions and types are allowed. Recursive types that would have infinite size are rejected by [05](05-types.md) (`INFINITE_SIZE_TYPE`). Cyclic type aliases are rejected by [05](05-types.md) (`ALIAS_CYCLE`). Cyclic constant initializers are rejected by const evaluation ([06](06-expressions-and-statements.md), D-EXPR-07). Cyclic supertraits are rejected by [05](05-types.md) (`SUPERTRAIT_CYCLE`). Cyclic `covers` clauses are rejected by [08](08-effects-and-capabilities.md) (`EFFECT_COVERS_CYCLE`).
- **Package cycles** are rejected by [package/02](../package/02-resolver.md).
- Resolution never depends on type information. The one exception is the type-relative tail of a path, which [05](05-types.md) resolves after Phase 4.

Diagnostics of this procedure are reported in source order (file path, then offset), as [impl/11](../impl/11-diagnostics.md) requires.

## 4.6 Visibility

### 4.6.1 Levels

There are three visibility levels (D-MOD-02). There is no module-private level.

| Level | Written | Visible from |
|---|---|---|
| package-private | nothing (default) | every module of the defining package |
| public | `pub` | every module of every package that can reach the item's module, directly or through a `pub use` |
| exported | `export` (functions only) | like `pub`; in addition, a core Wasm export of the root package (§4.6.6) |

`visible(e, M)` holds iff `e` is public or exported, or `pkg(e) = pkg(M)`. Visibility is checked at every use of a name, including the members reached by a path and the items of a `use` list. A failed check is `PRIVATE_ITEM` for items and members and `PRIVATE_FIELD` for fields. The diagnostic names the defining package and offers no fix outside the defining package.

`export` is allowed only on a function (`INVALID_EXPORT_TARGET`, 03). `pub export` is `REDUNDANT_PUB_EXPORT` (03).

### 4.6.2 Visibility of each kind of item and member

| Entity | Its visibility | What `pub` gives outside the package |
|---|---|---|
| function | its own marker | calling it; naming it as a value (fn item) |
| record type (`type T {..}`) | its own marker | naming the type; the `pub` fields, see §4.6.3 |
| record field | its own `pub` marker. A `pub` field of a non-`pub` type has no effect outside the package. | reading and writing the field (subject to D-CONTRACT-04, §4.6.5); naming it in construction and patterns |
| type alias, refined type | its own marker | naming it; it denotes its target. A refined type also gives its conversions ([05](05-types.md), [09](09-contracts-and-refinements.md)) |
| enum | its own marker | naming the type |
| enum variant, and every field of a record variant | **always that of the enum**; variants and their fields take no marker | constructing, matching, naming (`E.V`, `.V`) |
| distinct type | its own marker | naming the type. The constructor `D(b)`, the pattern `D(b)` and `.value` are public only with `= pub B`, see §4.6.4 |
| resource | its own marker | naming the type; fields as for records |
| trait | its own marker | naming it in bounds and impls; **all** its methods and associated types |
| trait item | that of the trait; trait items take no marker | |
| trait impl | none (impls have no visibility); `VISIBILITY_ON_TRAIT_IMPL_ITEM` (03) for markers on its items | the impl applies wherever its trait and its type can both be named |
| inherent impl item | its own `pub` marker; default package-private | calling it as `T.f(..)` or `x.f(..)` |
| constant | its own marker | its value |
| user effect | its own marker | naming it in effect clauses; it is opaque ([08](08-effects-and-capabilities.md) §8.4.3) |
| interface | its own marker | every member (§4.8.1) |
| interface member | that of the interface | |
| world | its own marker | referencing it in `include` and from another package's world |
| extern function | **always package-private**; `ExternItem` takes no marker (03 §3.10) | nothing; wrap it in a `pub fn` |
| `use` binding | private to the module that contains the `use` (N-A-7) | nothing |
| `pub use` binding | public | as for the re-exported item |

### 4.6.3 Fields outside the defining package

For a record type, resource or record variant `T` used in a module of another package (N-A-13):

- **Field access** `x.f` (read, write, or `borrow`) requires `f` to be visible. Otherwise the error is `PRIVATE_FIELD`.
- **Construction** `T(f1: e1, …)`, including the functional-update form `T(f1: e1, ..base)`, requires **every** field of `T` to be visible. A field that is not visible is `PRIVATE_FIELD`, even when it would be copied from `base`. Rationale: a type with private fields keeps control of every value of the type (P8, P5). Such types offer constructor functions.
- **Patterns** `T(f1: p1, …)` may name only visible fields. If `T` has any field that is not visible, the pattern MUST end in `..`. Without it the error is `PRIVATE_FIELD`, with the fix "add `..`".
- Record **variants** of an enum have the enum's visibility, so their fields are always visible wherever the enum is.

The same rules hold inside the defining package for fields of a type with an `invariant`, except that [09](09-contracts-and-refinements.md) restricts writes to the defining module (D-CONTRACT-04, §4.6.5).

```sai
// src/model.sai of acme:shop
pub type Order {
    pub id: U64
    total: I64
}

pub fn order(id: U64) -> Order {
    Order(id, total: 0)
}
```

```sai
// in another package
use acme:shop/model.{Order, order}

fn check(o: Order) -> U64 {
    let Order(id, ..) = o                 // OK: names only the visible field, ends in `..`
    id
}

fn make() -> Order {
    Order(id: 1, total: 0)                // rejected: PRIVATE_FIELD (`total`)
}
```

### 4.6.4 Distinct types

For `distinct D = B` (D-TYPE-03, D-MOD-02):

- `pub distinct D = B` publishes the type `D` and keeps its representation **opaque**. Outside the package, the constructor `D(b)`, the pattern `D(b)` and the field `.value` are not visible (`PRIVATE_ITEM` for the constructor and the pattern, `PRIVATE_FIELD` for `.value`). Impls of `D`, including derived ones, apply wherever `D` can be named.
- `pub distinct D = pub B` additionally publishes the constructor, the pattern and `.value`.
- `distinct D = pub B` without `pub` on `D` is the error `PUB_REPR_ON_PRIVATE_DISTINCT` (N-A-16). The marker could never take effect (P8).
- Inside the defining package, constructor, pattern and `.value` are always visible.

```sai
// src/ids.sai of acme:shop
pub distinct OrderId = U64                // opaque outside acme:shop
pub distinct Sku = pub U64                // transparent
distinct Internal = pub U32               // rejected: PUB_REPR_ON_PRIVATE_DISTINCT
```

### 4.6.5 Invariant fields

Fields of a type that has an `invariant` can be written only in the module that declares the type (D-CONTRACT-04). This is a restriction on top of visibility, defined with its diagnostic in [09](09-contracts-and-refinements.md). `invariant T { … }` MUST be in the module that declares `T` (`INVARIANT_OUTSIDE_DEFINING_MODULE`, 03).

### 4.6.6 `export` across packages

`export fn f` is a core Wasm export **only when its package is the root package of the build**, that is, the package whose artifact is being produced ([targets](../targets/targets-and-profiles.md)). In every other package, `export` behaves exactly like `pub` and produces no export (N-A-15). Rationale: a dependency must not be able to add entries to the export table of the program that uses it (P5, P8).

In the root package, `export` functions may be declared in any module. Their export names are their function names, or the string of `@export_name` ([15](15-attributes-and-lints.md)). Two exports with the same export name are `DUPLICATE_EXPORT_NAME` ([15](15-attributes-and-lints.md)). Scalar-only signatures are required by D-WASM-04 ([12](12-unsafe-and-ffi.md)).

### 4.6.7 Private names in public APIs

A public item MUST NOT mention a package-private entity in its **public surface**. A violation is the error `PRIVATE_TYPE_IN_PUBLIC_API` (N-A-14). For effects the error is `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` ([08](08-effects-and-capabilities.md) §8.4.2).

Here a public item is one that is `pub` or `export`. The surface of an item is the set of type, trait and constant names that appear in the following positions. Type parameters, `Self` and prelude names are always allowed. Names from other packages are always public, since they could not be named otherwise.

| Public item | Public surface |
|---|---|
| function, `pub` inherent method of a `pub` type | parameter types, return type, generic parameter bounds, `where` predicates, const parameter types |
| `pub` record type, resource | generic bounds and `where` predicates; the types of its `pub` fields (private fields may have private types) |
| `pub` enum | generic bounds and `where` predicates; every variant payload type |
| `pub distinct D = pub B` | `B` |
| `pub distinct D = B` (opaque) | nothing (`B` may be private) |
| `pub` alias `type A = T` | `T` |
| `pub` refined type `type R = B where p` | `B` (the predicate is checked by [09](09-contracts-and-refinements.md)) |
| `pub` constant | its type |
| `pub` trait | supertraits, associated type bounds, every method signature, generic bounds and `where` predicates |
| trait impl whose trait and self type are both public | the impl's trait reference, self type and `where` predicates |
| `pub` interface | every member signature ([13](13-components-and-wit.md) further restricts which types may appear) |

Expressions in `requires`, `ensures` and refinement predicates are not part of the surface defined here; [09](09-contracts-and-refinements.md) defines what they may reference.

```sai
// src/lib.sai of acme:shop
type Secret {
    key: U64
}

pub fn leak() -> Secret { Secret(key: 0) }      // rejected: PRIVATE_TYPE_IN_PUBLIC_API (`Secret`)

pub type Holder {
    secret: Secret                              // OK: private field
    pub size: USize
}
```

## 4.7 Scopes and shadowing

### 4.7.1 Scope structure

Scopes nest as follows. Each scope sees the bindings of every scope that encloses it.

| Scope | Introduced by | Bindings |
|---|---|---|
| prelude | always | §4.2.6 |
| module scope | each file | §4.4.1 |
| item generic scope | an `impl`, `trait`, type or function with `GenericParams` | its generic parameters; `Self` in traits and impls |
| function generic scope | a function's own `GenericParams` (nested in the impl or trait generic scope for methods) | its generic parameters |
| parameter scope | a function or closure | its parameters, including `self` |
| body block | the function or closure body | `let`/`var` bindings of the block |
| nested block | every `Block` | `let`/`var` bindings of the block |
| arm scope | each `match` arm | the bindings of the arm's pattern; visible in the guard and the arm body |
| condition scope | `if let`/`while let` conditions ([06](06-expressions-and-statements.md)) | the pattern bindings; visible in the then-block or loop body only |
| loop pattern scope | `for p in e` | the bindings of `p`; visible in the loop body only, fresh for each iteration |

Further rules:

- A `let` or `var` binding is in scope from the statement **after** its own statement to the end of the enclosing block. Its initializer cannot see it: `let x = x + 1` refers to an outer `x` (D-MOD-03).
- The bindings of `let p = e else { … }` are not in scope in the `else` block. They are in scope after the statement ([06](06-expressions-and-statements.md)).
- The body block of a function is **nested inside** its parameter scope, so a body binding may shadow a parameter: `fn f(name: Str) { let name = name.trim() }` is allowed (D-MOD-03).
- Generic parameters are in scope in the whole item: its parameters, return type, clauses, and body.
- Predicates of refined types and invariants, and contract clauses, have the scopes defined by [09](09-contracts-and-refinements.md). `requires` sees the parameters, and `ensures` also sees `result` and `old(..)`.

### 4.7.2 Shadowing rules

D-MOD-03 gives the rules:

1. A binding in an **inner** scope (block, match arm, closure, condition, loop pattern) may shadow a binding of the same name in an outer scope.
2. Declaring a name twice in the **same** block is the error `DUPLICATE_BINDING`. So are two parameters of the same name in one function or closure, and the same name twice in one pattern (03 §3.14). In a block, a name always denotes one binding (P8).
3. The alternatives of an or-pattern bind the same names, which is not a duplicate (`OR_PATTERN_BINDING_MISMATCH`, 03).

```sai
fn normalize(name: Str) -> String {
    let name = name.trim()                    // OK: body block shadows the parameter
    let upper = name.to_uppercase()
    if upper.is_empty() {
        let upper = String.from("ANON")       // OK: inner block
        return upper
    }
    let upper = upper.clone()                 // rejected: DUPLICATE_BINDING (same block)
    upper
}
```

### 4.7.3 Module aliases cannot be shadowed

A local binding MUST NOT have the name of a module alias of the module scope, that is, a `use` binding of a module or of an interface. Such a binding is the error `SHADOWS_MODULE` (D-MOD-01). The rule covers interface bindings as well as module bindings, because both are used as path roots (N-A-11). It applies to `let`, `var`, parameters, closure parameters and pattern bindings. Rationale: `client.send(x)` must not change meaning when a local `client` is introduced (P8).

```sai
use acme:app/http/client

fn run(client: I32) { }                       // rejected: SHADOWS_MODULE
```

### 4.7.4 Generic parameters

- Two generic parameters of one list with the same name are `DUPLICATE_BINDING`.
- A generic parameter of a method MUST NOT have the name of a generic parameter of its enclosing `impl` or `trait`. A violation is the error `SHADOWS_GENERIC_PARAM` (N-A-12). Rationale: inside the method, `T` would otherwise silently refer to a different parameter than in the impl header (P8).
- A generic parameter may have the name of a module-level type. Inside the item, the name denotes the parameter.

```sai
type Wrapper<T> {
    inner: T
}

impl<T> Wrapper<T> {
    fn replace<T>(self: Self, value: T) -> Self { self }    // rejected: SHADOWS_GENERIC_PARAM
}
```

## 4.8 Interfaces, worlds, effects and `include`

### 4.8.1 Interfaces (Stage 4)

- An `interface i { … }` declaration binds the lowercase name `i` in the module scope. Its **members** are the names of its `TypeDecl`, `EnumDecl`, `IfaceResource` and `FnHead` items. Two members with the same name are `DUPLICATE_BINDING`.
- Names inside an interface resolve in this order: the interface's own members, then the enclosing module scope, then the prelude (N-A-17). An interface member therefore shadows a module item of the same name within the interface. Which types an interface signature may *use* is restricted further by [13](13-components-and-wit.md) (D-WIT-01).
- From outside, interface members are reached through the interface binding (`processor.Image`, `processor.process`) or imported with an item list (`use acme:img/processor.{Image}`, where `processor` is declared in the root module of `acme:img`; §4.3.2 step 2.2).
- An imported WIT interface (`use wasi:http/outgoing-handler`) is a module alias whose members are the SAI names of its types, resources and functions, mapped from WIT names as [13](13-components-and-wit.md) defines. The binding name also names the interface effect ([08](08-effects-and-capabilities.md) §8.5).
- `impl i { … }` (`ImplTarget ::= ModPath`, 03 §3.7) resolves its `ModPath` in the module scope (§4.5.2). It must resolve to an interface; anything else is `IMPL_TARGET_NOT_INTERFACE` ([13](13-components-and-wit.md) §13.3.1). The items of the impl are matched by name against the interface's members. That matching, and the rule that a package has at most one impl per interface (D-WIT-01), are defined in [13](13-components-and-wit.md).
- Interface members are not module members. `use acme:img.{Image}` does not find a type `Image` that is a member of interface `processor`. It is `UNRESOLVED_IMPORT`.

### 4.8.2 Worlds (Stage 4)

- A `world w { … }` declaration binds the lowercase name `w` in the module scope. A world has no member namespace, and its items introduce no bindings into any module scope. To call an imported interface, a module must `use` it (D-WIT-03, [13](13-components-and-wit.md)). The only path through a world is `w.f` for a function that a world of the current package imports (§4.5.2 step 5).
- A `WorldRef` that is a `PKG_PATH` resolves like a `use` path (§4.3.2), except that it may also name a world of a WIT package or SAI package ([13](13-components-and-wit.md)). A `WorldRef` that is a `ModPath` resolves in the module scope of the module containing the world (§4.5.2). It must denote an interface (for `import`/`export`) or a world (for `include`). A world in `import` or `export`, and an interface or a whole package in `include`, is `INVALID_WORLD_ITEM` ([13](13-components-and-wit.md) §13.4.1); any other entity is `WRONG_NAME_KIND`. Naming a whole package in `import` or `export` is `WORLD_IMPORT_NEEDS_INTERFACE` (03).
- `include` merges another world into this one. Its semantics are in [13](13-components-and-wit.md). Name resolution only resolves the `WorldRef`.
- `export fn f(..)` inside a world is implemented by the function `f` of the **root module** of the same package (03 §3.9). The name is looked up in the root module's member namespace `X(root)`. That namespace holds the root module's declared functions and its `pub use` re-exports, so a function defined in another module can be exported through `pub use` (N-A-17). If no function `f` is found, the error is defined by [13](13-components-and-wit.md).
- The world named by `export world` in `sai.package` is looked up in `X(root)` of the package ([package/01](../package/01-package-contract.md)).

### 4.8.3 Effects (Stage 3)

`EffectRef` resolution is defined in [08](08-effects-and-capabilities.md) §8.2.2. In terms of this chapter:

- The built-in effect roots are matched first, by spelling, and take precedence over every binding (D-EFF-03).
- Otherwise the `EffectRef` is a path resolved by §4.5.2, starting at the module scope (step 3 of §4.5.1). It must end at a user `effect` declaration or at an interface binding. Anything else is `UNKNOWN_EFFECT` ([08](08-effects-and-capabilities.md)).
- A user effect is a module-level item and follows §4.6. A package-private effect can be named from every module of its package. Outside the package, a `pub` effect can be named but not inspected ([08](08-effects-and-capabilities.md) §8.4.3).
- Declaring a user effect whose name is a built-in root is `EFFECT_NAME_RESERVED` ([08](08-effects-and-capabilities.md)).

```sai
// src/db.sai of acme:app
pub effect write
    covers { net.connect }

// src/lib.sai of acme:app
use acme:app/db

pub fn save(v: I32)
    effects { db.write }
{
}
```

## 4.9 Canonical paths

### 4.9.1 Definition

Every nameable entity other than a local binding has exactly one **canonical path** (N-A-18). Canonical paths are used in diagnostics, generated documentation, semantic diffs and symbol IDs ([impl/12](../impl/12-semantic-tooling.md)). They are a notation of the toolchain, not source syntax.

```text
CanonicalPath ::= PackageId ModulePart? Component*
PackageId     ::= PkgSeg ':' PkgSeg                    (* 02 §2.9 *)
ModulePart    ::= ( '/' Segment )+                     (* module path; absent for the root module *)
Component     ::= '.' Name                             (* item or member *)
                | '.' 'impl[' ImplHeader ']'           (* trait or interface impl *)
                | '.' 'field[' ( Name | INT ) ']'      (* field; INT for a tuple-variant payload *)
                | '.' 'generic[' Name ']'              (* generic parameter *)
                | '.' 'test[' STRING ']'               (* test declaration *)
                | '.' 'property[' STRING ']'           (* property declaration *)
ImplHeader    ::= CanonType ' for ' CanonType          (* trait impl: trait, then self type *)
                | CanonicalPath                        (* interface impl: the interface *)
```

| Entity | Canonical path |
|---|---|
| package | `ns:pkg` |
| module | `ns:pkg/s1/…/sn`; the root module is `ns:pkg` |
| module-level item `x` of module `m` | `m.x` |
| enum variant `V` of `E` | `E.V` (the enum's path, then `.V`) |
| field `f` of a record, resource or record variant `T` | `T.field[f]` |
| payload `i` of a tuple variant `V` | `E.V.field[i]` |
| inherent associated function or method `f` of `T` | `T.f`, whatever module the inherent impl is in (inherent item names are unique per type, [05](05-types.md)) |
| trait method or associated type `x` of trait `Tr` | `Tr.x` |
| trait impl in module `m` | `m.impl[Tr<..> for T<..>]` |
| item `x` of a trait impl | `m.impl[…].x` |
| interface impl in module `m` | `m.impl[I]`, where `I` is the interface's canonical path |
| interface member `x` of interface `i` | `i.x`; a resource method `i.R.f` |
| generic parameter `T` of item `p` | `p.generic[T]` |
| `test "name"` / `property "name"` in module `m` | `m.test["name"]` / `m.property["name"]` (STRING with the escapes of 02 §2.6.4, whose `Escape` forms are listed in §2.6.3) |
| extern function `f` declared in module `m` | `m.f` |
| item of a WIT package (Stage 4) | `ns:pkg/iface.x`, where `iface` is the WIT interface name and `x` the SAI name of the member ([13](13-components-and-wit.md)) |

`CanonType` prints a type as follows: a nominal type by its canonical path followed by its generic arguments in `<…>`, separated by `, `; a type parameter by its name; a primitive by its name (`I32`, `Int` is printed `I64`); and every structural type in source syntax with canonical paths inside (`(A, B)`, `Array<T, 4>`, `borrow T`, `borrow mut T`, `fn(A) -> B effects {…}`, with effects in the canonical order of [08](08-effects-and-capabilities.md)). Aliases are expanded. `owned T` is printed as `T`. Regions are never printed.

Consequences:

- **Re-exports do not create paths.** An entity reached through `pub use` has the canonical path of its definition (D-MOD-04).
- **Primitive and structural types have no canonical path.** Their name, or their printed form, is their identity.
- **Fields are tagged** with `field[..]`, so a field and a method with the same name have different paths.
- A trait impl is identified by its header. Coherence makes the header unique ([05](05-types.md)).

```text
acme:shop                                     root module
acme:shop/model                               module
acme:shop/model.Order                         record type
acme:shop/model.Order.field[total]            field
acme:shop/model.Order.create                  inherent associated function
acme:shop/model.Shape.Rect.field[width]       field of a record variant
acme:shop/model.impl[sai:core/fmt.Format for acme:shop/model.Order].fmt
acme:user.User.create                         item of the root module of acme:user
```

### 4.9.2 Mapping to symbol IDs

A symbol ID has the form `symbol://<ns>%3A<pkg>/<module path>/<item>[/<member>]#<6 hex digits>` (D-TOOL-02). The `%3A` encodes the package separator as a byte in the URI authority; a literal colon there would be interpreted as a port delimiter. It is built from the canonical path by:

1. writing `symbol://` and the `PackageId`, with its namespace/package separator encoded as `%3A`;
2. for a non-root module, appending `/` and the module path. For the root module, nothing is appended (C-08);
3. appending `/` and each `Component` without its leading `.`, percent-encoded as in [impl/12](../impl/12-semantic-tooling.md) §12.2.2;
4. appending `#` and the persistent hex identifier ([impl/12](../impl/12-semantic-tooling.md) §12.2.3).

So `acme:user.User.create` becomes `symbol://acme%3Auser/User/create#8F21A7` (C-08). The identity of a symbol is the pair of its package node (a package identity and a compatibility line, [package/02](../package/02-resolver.md) §2.1) and its hex part; within one package node no two symbols have the same hex part. The hex part is computed from the canonical path when the symbol is first seen and persists across renames and moves ([impl/12](../impl/12-semantic-tooling.md) §12.2.3, §12.2.4, N-H-65). The path part is descriptive and changes on rename or move ([impl/12](../impl/12-semantic-tooling.md) §12.2.2).

## 4.10 Stages

| Feature | Stage |
|---|---|
| module tree, root module, `use` of modules of the current package and of `sai:core` / `sai:alloc`, item lists, `pub use`, visibility, scopes, shadowing, canonical paths | 0 |
| `use` of other packages, dependencies, user effects in effect position | 3 |
| interfaces, WIT packages, worlds, `include`, `impl` of an interface | 4 |
| `extern "C"` items | 7 |

A program that uses a feature of a stage the toolchain does not implement is rejected with `FEATURE_NOT_IMPLEMENTED`, naming the stage ([01](01-introduction.md)).

## 4.11 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-A-1 | The package identity always comes from the `package` field of `sai.package`, including in toolchains that implement only Stages 0–2. Those may ignore the other fields (§4.2.1). | One source of identity (P6); `use` of the current package needs it from Stage 0. |
| N-A-2 | The module tree is computed by §4.2.3: byte-ordered walk, entries starting with `.` and non-`.sai` files ignored, symbolic links rejected (`SYMLINK_IN_SOURCE`), segments must match `[a-z][a-z0-9_]*` and be neither strict keywords, reserved words nor `lib` (`INVALID_MODULE_NAME`), and a directory without a sibling file is a namespace only. | Deterministic, file-system-independent builds (P2, P6, P8). |
| N-A-3 | A submodule is not a member of its parent module. Each module is imported by its own `use` (§4.2.5). | One `use` names one module (P1); moving a file cannot change other paths (P8). |
| N-A-4 | A `PKG_PATH` names a module if one exists, otherwise an interface declared in the parent module. A module file and an interface with the same path are `MODULE_INTERFACE_CONFLICT`. SAI segments are matched exactly; WIT paths must name one interface (`INVALID_USE_TARGET`) (§4.3.2). | Every path has exactly one meaning (P8); kebab mapping is only for WIT and package names (P4). |
| N-A-5 | The binding name of a `use` is the alias, else the last segment, else the package name, with `-` mapped to `_`. A keyword or reserved word requires an alias (`USE_BINDING_IS_KEYWORD`) (§4.3.3). | Deterministic names (P8); matches D-MOD-01. |
| N-A-6 | `use P.{…}` without an alias binds only the items. `use P as a.{…}` binds both (§4.3.4). | Matches D-MOD-01 and 03 §3.4; no implicit bindings (P1, R-01). |
| N-A-7 | A module's member namespace contains its declared items and its `pub use` bindings. Plain `use` bindings are private to the module (§4.4.2, §4.6.2). | Imports stay local; only `pub use` publishes (P1, D-MOD-04). |
| N-A-8 | `DUPLICATE_BINDING` is the code for every duplicate name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3). | One rule, one code (P7); consistent with D-MOD-03 and [08](08-effects-and-capabilities.md) §8.4.1. |
| N-A-9 | Primitive type names, `Int`, `UInt` and `Never` cannot be rebound, and the reserved names `Send` and `Share` ([11](11-async-and-concurrency.md) N-I-19) cannot be declared or used as import names or aliases (`RESERVED_TYPE_NAME`). Other prelude names may be rebound at module level with the warning `SHADOWS_PRELUDE` (§4.4.4). | `I32` means the same everywhere (P8); prelude growth, and the later addition of the auto traits `Send` and `Share`, must not break code (P8). |
| N-A-10 | Unqualified lookup order is locals, generic parameters, module scope, prelude. Resolution runs in the four phases of §4.5.4, so module cycles are allowed (§4.5.1, §4.5.4). | Predictable and order-independent (P8, P2). |
| N-A-11 | `SHADOWS_MODULE` also applies to bindings of interfaces (§4.7.3). | Interface bindings are path roots like module aliases (P8). |
| N-A-12 | A method's generic parameter MUST NOT reuse the name of a generic parameter of the enclosing impl or trait (`SHADOWS_GENERIC_PARAM`) (§4.7.4). | Avoids silently different `T`s in one item (P8). |
| N-A-13 | Outside its package, a record or resource can be constructed (also with `..base`) only if all its fields are visible. Patterns may name only visible fields and need `..` if any field is not visible (§4.6.3). | The defining package controls all values of types with private fields (P5, P8). |
| N-A-14 | The public surface checked by `PRIVATE_TYPE_IN_PUBLIC_API` is the table of §4.6.7. | Every public signature can be written by its users (P1). |
| N-A-15 | `export` creates a core Wasm export only in the root package of the build. In dependencies it means `pub` (§4.6.6). | A dependency cannot add exports (P5, P8). |
| N-A-16 | `distinct D = pub B` with `D` not `pub` is the error `PUB_REPR_ON_PRIVATE_DISTINCT` (§4.6.4). | A marker that cannot take effect is a mistake (P8). |
| N-A-17 | Inside an interface, names resolve in its members, then the module scope, then the prelude. A world's `export fn f` and `export world` in `sai.package` are looked up in the member namespace of the root module (§4.8). | Interface-local types (R-08); re-exports allow facades (P9). |
| N-A-18 | Canonical paths have the form of §4.9.1, with `field[..]`, `impl[..]`, `generic[..]`, `test[..]` and `property[..]` components, and map to symbol IDs by §4.9.2. | One unambiguous identity per entity (P3). |
| N-A-19 | A `use` whose target is the current module is the error `USE_OF_CURRENT_MODULE` (§4.3.2). | It can only create confusing duplicate names (P8). |
| N-A-20 | `use` item lists name only module-level items. Variants and associated items cannot be imported (§4.3.4). | Members stay qualified (`Shape.Circle`), as D-SYN-02 intends (P1). |
| N-A-67 | Segments after a constant are field accesses and method calls on its value, as after a local binding. Only a function, effect or world followed by segments is `WRONG_NAME_KIND`, except `w.f` for an imported function `f` of a world `w` of the current package (§4.5.2 step 5). | An associated constant already continues with methods (`I32.MAX.abs()`); a module constant works the same, without a parenthesized workaround (P9). |

## 4.12 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `MISSING_ROOT_MODULE` | error | `<source>` or `<source>/lib.sai` does not exist |
| `SYMLINK_IN_SOURCE` | error | a symbolic link inside `<source>` |
| `INVALID_MODULE_NAME` | error | a directory or file name in `<source>` is not a valid module segment, or is `lib` outside the root file |
| `NON_ASCII_PUBLIC_NAME` | error | defined in [02](02-lexical-structure.md) §2.4; here: a non-ASCII file or directory name |
| `UNKNOWN_PACKAGE` | error | the package of a `PKG_PATH` is neither the current package, `sai:core`, `sai:alloc`, a WASI package of the toolchain, nor a declared dependency |
| `INACTIVE_DEPENDENCY` | error | a `use` of a dependency whose `only` list does not contain the build target (§4.3.2 step 1.3, [package/01](../package/01-package-contract.md) §1.10.5 rule 4) |
| `UNKNOWN_MODULE` | error | no module or interface has the given path |
| `INVALID_USE_TARGET` | error | a WIT `PKG_PATH` names a package or a world instead of an interface |
| `UNKNOWN_WIT_ITEM` | error | defined in [13](13-components-and-wit.md) §13.7.2; here: the interface segment of a WIT `PKG_PATH` matches nothing (§4.3.2 step 3.2) |
| `MODULE_INTERFACE_CONFLICT` | error | an interface and a module file have the same path |
| `USE_OF_CURRENT_MODULE` | error | a `use` names the module that contains it |
| `USE_BINDING_IS_KEYWORD` | error | the derived binding name of a `use` is a keyword or reserved word; an alias is required |
| `UNRESOLVED_IMPORT` | error | an item of a `use` list is not in the target's member namespace |
| `ALIAS_CASE_MISMATCH` | error | an item alias has a different case class from the item |
| `PUB_USE_WITHOUT_ITEMS` | error | a `pub use` has no item list |
| `PRIVATE_REEXPORT` | error | a `pub use` lists an item that is not `pub` |
| `REEXPORT_CYCLE` | error | a chain of `pub use` re-exports returns to itself (D-MOD-04) |
| `UNUSED_IMPORT` | warning (lint) | a non-`pub` `use` binding is never referenced (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `REDUNDANT_IMPORT` | warning (lint) | a `use` binding denotes what its name already denotes (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `DUPLICATE_BINDING` | error | two bindings of one name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3, D-MOD-03, N-A-8) |
| `RESERVED_TYPE_NAME` | error | a declaration or import alias uses a primitive type name, `Int`, `UInt`, `Never`, `Send` or `Share` (§4.4.4) |
| `SHADOWS_PRELUDE` | warning (lint) | a module-level binding shadows a prelude name (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `UNRESOLVED_NAME` | error | a name or path segment does not resolve |
| `WRONG_NAME_KIND` | error | a name resolves to an entity of the wrong kind for its position |
| `IMPL_TARGET_NOT_INTERFACE` | error | defined in [13](13-components-and-wit.md) §13.3.1; here: the path of `impl i` is not an interface (§4.8.1) |
| `INVALID_WORLD_ITEM` | error | defined in [13](13-components-and-wit.md) §13.4.1; here: a `WorldRef` names a world where an interface is required, or the reverse (§4.8.2) |
| `EXPECTED_VALUE_FOUND_TYPE` | error | defined in [06](06-expressions-and-statements.md) §6.4.1 |
| `PRIVATE_ITEM` | error | an item, member, distinct constructor or pattern is not visible here |
| `PRIVATE_FIELD` | error | a field (or `.value` of an opaque distinct type) is not visible here, or construction/pattern rules of §4.6.3 are violated |
| `PUB_REPR_ON_PRIVATE_DISTINCT` | error | `distinct D = pub B` where `D` is not `pub` |
| `PRIVATE_TYPE_IN_PUBLIC_API` | error | a public item's surface names a package-private type, trait or constant |
| `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` | error | defined in [08](08-effects-and-capabilities.md) §8.4.2 |
| `SHADOWS_MODULE` | error | a local has the name of a module or interface binding (D-MOD-01) |
| `SHADOWS_GENERIC_PARAM` | error | a method's generic parameter reuses a name of its impl's or trait's generic parameters |
| `SELF_OUTSIDE_IMPL` | error | defined in [05](05-types.md) §5.2.1 |
| `UNDECLARED_LABEL` | error | defined in [06](06-expressions-and-statements.md) §6.16.4 |
| `LABEL_SHADOWED` | error | defined in [06](06-expressions-and-statements.md) §6.16.4 |
| `UNKNOWN_EFFECT` | error | defined in [08](08-effects-and-capabilities.md) §8.2.2 |
| `EFFECT_NAME_RESERVED` | error | defined in [08](08-effects-and-capabilities.md) §8.4.1 |
| `EFFECT_COVERS_CYCLE` | error | defined in [08](08-effects-and-capabilities.md) §8.4.1 |
| `DUPLICATE_FIELD` | error | defined in [05](05-types.md) §5.6.1 |
| `DUPLICATE_VARIANT` | error | defined in [05](05-types.md) §5.6.2 |
| `DUPLICATE_INHERENT_ITEM` | error | defined in [05](05-types.md) §5.9.2 |
| `DUPLICATE_TRAIT_ITEM` | error | defined in [05](05-types.md) §5.8.1 |
| `ALIAS_CYCLE` | error | defined in [05](05-types.md) §5.6.5 |
| `INFINITE_SIZE_TYPE` | error | defined in [05](05-types.md) §5.6.7 |
| `SUPERTRAIT_CYCLE` | error | defined in [05](05-types.md) §5.8.1 |
| `DUPLICATE_EXPORT_NAME` | error | defined in [15](15-attributes-and-lints.md) §15.7 |
| `INVALID_EXPORT_TARGET` | error | defined in [03](03-grammar.md) §3.3 |
| `REDUNDANT_PUB_EXPORT` | error | defined in [03](03-grammar.md) §3.3 |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | defined in [03](03-grammar.md) §3.7 |
| `OR_PATTERN_BINDING_MISMATCH` | error | defined in [03](03-grammar.md) §3.14 |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | defined in [03](03-grammar.md) §3.8 |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | defined in [03](03-grammar.md) §3.9 |
| `EXPECTED_TOKEN` | error | defined in [03](03-grammar.md) §3.16 |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5 |
