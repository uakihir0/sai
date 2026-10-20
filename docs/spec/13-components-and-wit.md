# 13. Components and WIT

**Status:** Normative
**Stage:** 4 (`future` / `stream` and async exports: Stage 5)
**Related:** [03-grammar.md](03-grammar.md) §3.8–3.9, [04-modules-and-names.md](04-modules-and-names.md), [05-types.md](05-types.md), [07-ownership-and-borrowing.md](07-ownership-and-borrowing.md), [08-effects-and-capabilities.md](08-effects-and-capabilities.md), [09-contracts-and-refinements.md](09-contracts-and-refinements.md), [11-async-and-concurrency.md](11-async-and-concurrency.md), [../impl/07-component-lowering.md](../impl/07-component-lowering.md), [../package/01-package-contract.md](../package/01-package-contract.md), [../targets/targets-and-profiles.md](../targets/targets-and-profiles.md), [decisions.md](../design/decisions.md) D-WIT-01..03, D-OWN-06, D-TGT-01, D-LEX-02..03

This chapter defines how SAI programs meet the WebAssembly Component Model: the declaration of `interface`, `world` and interface `resource` items, the implementation of interfaces with `impl <interface>`, the binding of imported WIT packages into SAI, the conversion of names between SAI identifiers and WIT kebab-case, the complete type mapping in both directions, the WIT-compatibility rule, the semantics of resources and handles at the boundary, the exact text of generated WIT, the meaning of `export world` and what a component exports and imports, the relation between worlds and targets, and the effects and capabilities attached to the boundary. The syntax is in [03-grammar.md](03-grammar.md) §3.8–3.9 and the `export world` declaration of `sai.package` is in [package/01](../package/01-package-contract.md). The Canonical ABI lowering is in [impl/07](../impl/07-component-lowering.md).

---

## 13.1 Overview and terminology

### 13.1.1 Model

- The internal ABI of SAI is private; the public boundary of a SAI component is the Component Model, its Canonical ABI and WIT (draft §40–41, P4). Nothing about SAI's internal layout or calling convention is observable through a component.
- A SAI package can declare WIT interfaces and worlds in SAI syntax, implement interfaces, and consume WIT written by others. The compiler reads WIT directly; no generated bindings are needed (D-WIT-02).
- Every SAI interface maps to exactly one WIT interface, every SAI world to exactly one WIT world, and every interface resource to exactly one WIT resource (D-WIT-01, D-OWN-06, draft §43). The mapping is total on the accepted subset and deterministic.

### 13.1.2 Terminology

| Term | Meaning |
|---|---|
| WIT package | A WIT package `ns:name@version`, either produced by a SAI package (§13.11) or consumed from a dependency or the toolchain (§13.7). |
| local interface / world | An `interface` / `world` declared in SAI source of the current package. |
| external interface / world | An interface or world of another WIT package, named by a `PKG_PATH`. |
| SAI view | The SAI declarations through which an external WIT item is visible (§13.7). `sai bindgen wit --show <pkg>` prints it (D-WIT-02). |
| boundary type | A type that appears in the signature of an interface function, world function, or resource member. |
| export world | The world named by `export world` in the root `sai.package` ([package/01](../package/01-package-contract.md)). |
| component build | A build that produces a component: every build for `wasm32-wasi` / `wasm64-wasi`, and `sai component build` for any target (D-TGT-01). |
| glue | Compiler-generated code that converts between the Canonical ABI and SAI values ([impl/07](../impl/07-component-lowering.md)). |

---

## 13.2 Interfaces

### 13.2.1 Declaration

An interface is declared with `InterfaceDecl` ([03 §3.9](03-grammar.md)). Its name is a lower identifier (D-LEX-02) and MUST be ASCII (D-LEX-03, `NON_ASCII_PUBLIC_NAME`). An interface is an item of its module and shares the module namespace (D-SYN-03). It may be `pub` (visible to other modules and packages per [04-modules-and-names.md](04-modules-and-names.md)); the visibility of an interface affects only SAI-level naming, never whether it can be exported by a world of its own package.

An interface contains, in any order:

| Item | Syntax | WIT |
|---|---|---|
| record | `type R { fields }` | `record r { … }` |
| alias | `type A = T` (no refinement clause) | `type a = t;` |
| enum (all variants unit) | `enum E { A  B }` | `enum e { a, b }` |
| variant (some variant has a payload) | `enum V { A(T)  B }` | `variant v { a(t), b }` |
| flags | `@flags enum F { A  B }` | `flags f { a, b }` |
| resource | `resource R { FnHead* }` | `resource r { … }` |
| function | `FnHead` | `name: func(…) -> …;` |

The items of an interface are its members; they are named from outside the interface as `iface.Item` and `iface.function`. Interface members have no individual visibility: they are visible wherever the interface is.

### 13.2.2 Restrictions on interface items

The following are errors inside an interface, each reported at the offending construct:

| Construct | Code |
|---|---|
| `GenericParams` on a type, enum, resource member or function; a `where` clause | `GENERIC_INTERFACE_ITEM` |
| a refinement clause on an alias (already excluded by [03 §3.9](03-grammar.md)) | `WIT_INCOMPATIBLE_TYPE` |
| a `borrows`, `requires` or `ensures` clause on an interface function or resource member | `INTERFACE_FN_CLAUSE` |
| an `effects` clause on an interface function or resource member | `EFFECTS_ON_INTERFACE_FN` ([08 §8.6.1](08-effects-and-capabilities.md)) |
| `unsafe fn` | `INTERFACE_FN_CLAUSE` |
| `async fn` (Stage 5; until then `FEATURE_NOT_IMPLEMENTED`) | `FEATURE_NOT_IMPLEMENTED` |
| a body on an interface function or resource member | `INTERFACE_FN_BODY` |
| an explicit discriminant `= INT` on a variant | `WIT_EXPLICIT_DISCRIMINANT` |
| a record without fields, an enum or flags without variants | `WIT_EMPTY_TYPE` |
| a variant payload that is not exactly one type (`A(T, U)` or a record variant `A(x: T)`) | `WIT_INCOMPATIBLE_TYPE` (help: use `A((T, U))` or declare a record) |
| `@flags` on an enum with a payload variant, an explicit discriminant, or more than 32 variants (reported instead of `WIT_EXPLICIT_DISCRIMINANT`, §13.9.4) | `INVALID_FLAGS_ENUM` |
| `@repr` on any interface type | `WIT_INCOMPATIBLE_TYPE` |
| a boundary type that is not WIT-compatible (§13.9) | `WIT_INCOMPATIBLE_TYPE` |

- Contracts are not part of an interface because WIT cannot carry them (P4). An implementation may add `requires` and `ensures` to the functions of its `impl`; for exported functions they are checked at run time at the trust boundary ([09-contracts-and-refinements.md](09-contracts-and-refinements.md), D-CONTRACT-01).
- Fields of an interface record are always public. A `pub` marker on such a field is permitted and has no effect.
- `@derive` is permitted on interface records, enums and variants (D-TYPE-08). Interface resources can never be `Copy` (D-OWN-06).
- Doc comments on interface items are carried into generated WIT (§13.11.2).

### 13.2.3 Using other interfaces' types

A boundary type may name a type of another interface as `other.T` (D-WIT-01). `other` must resolve to a local interface or to the binding of an external interface (§13.7.2). This corresponds to WIT `use other.{t}` (§13.11.3). A cycle of such references between local interfaces is the error `INTERFACE_USE_CYCLE`, because WIT forbids cyclic `use` between interfaces.

### 13.2.4 Interface resources

An interface resource is declared with `IfaceResource` ([03 §3.9](03-grammar.md)). It is an **abstract** resource: the interface declares its members; an `impl` of the interface binds it to a concrete resource type (§13.3.3). Its members are classified ([03 §3.9](03-grammar.md)):

1. A function whose first parameter is a `SelfParam` is a **method**. The self type MUST be `borrow Self` or `borrow mut Self`; `self: Self` (consuming the handle) is `WIT_INCOMPATIBLE_TYPE`, because WIT methods always take `borrow<r>`.
2. A function named `new` without a `SelfParam` whose return type is exactly `Self` or the resource's own name is the **constructor**. At most one constructor exists per resource.
3. Every other function is a **static** function (including a `new` whose return type is, for example, `Result<Self, E>`).

Within the resource, `Self` and the resource's name denote the resource type. Outside the resource but inside the interface, the resource is named by its plain name (`Image`); outside the interface, by `iface.Image`.

---

## 13.3 Interface implementations

### 13.3.1 Form and target

An interface implementation is an `ImplDecl` whose `ImplTarget` is a `ModPath` ending in a lower identifier ([03 §3.8](03-grammar.md)). The path MUST resolve to a local interface or to the binding of an external interface (§13.7.2); otherwise `IMPL_TARGET_NOT_INTERFACE`.

- A package contains at most one `impl` per interface (D-WIT-01). A second one, anywhere in the package, is `DUPLICATE_INTERFACE_IMPL`, reported at the later one in module-path order, then source order.
- Implementing an external interface is how a package exports it; for example `impl run` after `use wasi:cli/run` (§13.6.3).
- The impl contains exactly one `AssocTypeDef` per resource of the interface (§13.3.3) and exactly one `FnDecl` per function of the interface. A missing item is `INTERFACE_IMPL_MISSING_ITEM`; an item that the interface does not declare is `INTERFACE_IMPL_EXTRA_ITEM`.
- Functions of an interface impl cannot be `pub` (`VISIBILITY_ON_TRAIT_IMPL_ITEM`, [03 §3.8](03-grammar.md)) and cannot be generic (`GENERIC_INTERFACE_ITEM`).
- Members of the interface are **not** in scope unqualified inside the impl (P1): the impl writes `processor.ImageError`, as in D-WIT-01.

### 13.3.2 Signature conformance

A function of the impl conforms to the interface function of the same name when:

1. the number of parameters is the same and each parameter type is the same type, after expanding aliases and replacing each abstract resource `iface.R` by its bound concrete type (§13.3.3);
2. the return types are the same type under the same replacement (an omitted return type is `()`);
3. `async` agrees (Stage 5);
4. the impl function is not `unsafe`.

Parameter names need not match; the names of the interface declaration are the WIT names. `var` on parameters is permitted. The impl function may add `effects`, `requires` and `ensures` clauses. A non-conforming function is `INTERFACE_IMPL_SIGNATURE_MISMATCH`, which shows both signatures.

Functions of an impl whose interface is exported by the export world are **exposed** functions: they MUST declare a non-empty effect set ([08 §8.9.3](08-effects-and-capabilities.md), `MISSING_EFFECTS_DECLARATION`).

### 13.3.3 Binding resources

`type R = C` inside an interface impl binds the abstract resource `iface.R` to the concrete type `C` (D-WIT-01). `C` MUST be a non-generic `resource` type declared in the same package ([07-ownership-and-borrowing.md](07-ownership-and-borrowing.md), D-TYPE-03); otherwise `INVALID_RESOURCE_BINDING`. A concrete resource type may be bound to at most one abstract resource in the whole package (one SAI resource is one WIT resource, D-OWN-06); a second binding is `INVALID_RESOURCE_BINDING`. A resource of an external interface also cannot be bound when it occurs in a record field, a variant payload or a type alias of a WIT interface visible to the package: such a type denotes the imported resource in every SAI view, so it would carry imported handles where the implementation expects `C` (`INVALID_RESOURCE_BINDING`, [impl/16](../impl/16-interfaces-and-wit.md) §16.7.3).

The members of the abstract resource are implemented by **inherent associated functions of `C`** with the same names (N-D-28):

- each method by an inherent method of `C` whose self parameter has the same form (`borrow Self` or `borrow mut Self`);
- the constructor by an inherent associated function `new` of `C` returning `C` (or `Self`);
- each static function by an inherent associated function of `C` without a self parameter.

Conformance is checked as in §13.3.2 with `Self` replaced by `C`. These functions need not be `pub`; exporting the interface exposes them through the component regardless of their SAI visibility. They are exposed functions in the sense of §13.3.2. A missing member is `INTERFACE_IMPL_MISSING_ITEM`, reported at the binding `type R = C`.

If `C` has a `Drop` impl, it runs when the component's client drops the last owned handle (§13.10.4).

Example (accepted). It completes D-WIT-01 with valid bodies:

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

resource MyImage {
    data: Bytes
    pixel_width: U32
}

impl MyImage {
    fn new(data: Bytes) -> MyImage {
        return MyImage(data: data, pixel_width: 0)
    }

    fn width(self: borrow Self) -> U32 {
        return self.pixel_width
    }
}

impl processor {
    type Image = MyImage

    fn resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, processor.ImageError>
        effects { alloc }
    {
        if width == 0 or height == 0 {
            return Err(processor.ImageError.Unsupported)
        }
        if image.len() > 16_000_000 {
            return Err(processor.ImageError.TooLarge)
        }
        return Ok(image)
    }
}

world image_service {
    import wasi:http/outgoing-handler
    export processor
}
```

`MyImage.new` and `MyImage.width` have the empty effect set, so they need no clause. `resize` returns its argument but is declared `effects { alloc }` deliberately, to keep room for a real implementation (an exposed function may over-declare, [08 §8.9.2](08-effects-and-capabilities.md)).

### 13.3.4 Calling interface functions

Within a package, a call `iface.f(…)` (or a resource member call) resolves as follows (N-D-29):

1. If the package contains `impl iface`, the call is an ordinary call of the impl function (or of the inherent function of the bound resource type). It has the effects of that function ([08 §8.7.2](08-effects-and-capabilities.md)) and produces no component import.
2. Otherwise the call is a call of an **imported** function. It has the interface effect `ns:pkg/iface` ([08 §8.5](08-effects-and-capabilities.md)), and the interface becomes an import of the component if the call is reachable.

Consequently every interface of another package, whether that package is a SAI source package or a binary component, is always reached as an imported interface, across a Canonical ABI boundary. The build satisfies it in one of three ways: within the module, when a hybrid dependency in source form provides it (§13.5.2, N-D-104); by composition with an artifact; or as an import of the component, satisfied by the host. Library-style reuse of another SAI package's code uses its ordinary `pub` functions, not its interfaces.

---

## 13.4 Worlds

### 13.4.1 Declaration and items

A world is declared with `WorldDecl` ([03 §3.9](03-grammar.md)). Its name is a lower identifier and MUST be ASCII. Its items are:

| Item | Meaning | WIT |
|---|---|---|
| `import i` | the world imports interface `i` | `import i;` |
| `export i` | the world exports interface `i` | `export i;` |
| `import FnHead` | the world imports a free function | `import f: func(…);` |
| `export FnHead` | the world exports a free function | `export f: func(…);` |
| `include w` | the world contains every item of world `w` | `include w;` |

- `import` and `export` of interfaces are per interface (D-WIT-03, C-01). A `PKG_PATH` without an interface segment is `WORLD_IMPORT_NEEDS_INTERFACE` ([03 §3.9](03-grammar.md)). A `WorldRef` that resolves to a world in an `import`/`export`, or to an interface or a whole WIT package in an `include`, is `INVALID_WORLD_ITEM`. A world of another package can only be named by a `PKG_PATH`, because `use` never binds a world ([04 §4.3.2](04-modules-and-names.md)).
- A world `FnHead` follows the rules for interface functions (§13.2.2): no generics, no clauses, no body. Its boundary types may name types of interfaces (`iface.T`) and primitives; a world declares no types of its own.
- The implementation of `export FnHead` is the function of the same name in the root module (`lib.sai`) of the package ([03 §3.9](03-grammar.md)). If there is none, `WORLD_EXPORT_UNIMPLEMENTED`; if its signature does not conform (§13.3.2), `INTERFACE_IMPL_SIGNATURE_MISMATCH`. The implementing function is an exposed function.
- Each local interface named by `export` MUST have an `impl` in the package, and each external interface named by `export` MUST have an `impl` of its binding; otherwise `WORLD_EXPORT_UNIMPLEMENTED`.
- A function imported with `import FnHead` is called as `world_name.f(…)` from any module of the package that can name the world. `world_name` MUST be a world of the current package, and `f` is looked up among the `import FnHead` items of that world and of the worlds it includes, the first in elaboration order winning. Its effect is the world-import effect of [08 §8.5.1](08-effects-and-capabilities.md).

### 13.4.2 Elaboration

The **elaborated** world `elab(W)` is computed as follows:

1. Start with the items of `W`.
2. Replace each `include w` by the items of `elab(w)`, recursively. An include cycle is `WORLD_INCLUDE_CYCLE`.
3. For every exported interface `e` and every interface `u` whose types `e` uses (§13.2.3), transitively: if `u` is not exported by the world, add `import u` (WIT implicit import). If `u` is exported, the use refers to the exported instance.
4. For every imported interface, add `import u` for every interface it uses, transitively.

After elaboration:

- The same interface MUST NOT be both imported and exported, and no interface or function name MAY appear twice among the imports or twice among the exports; otherwise `WORLD_DUPLICATE_ITEM` (N-D-30). Duplicates that arise only through `include` of the same item are merged, not reported.
- Two different versions of one external interface cannot coexist in one world; this cannot arise in source, because the version of every external interface is fixed by resolution (§13.7.1).

---

## 13.5 `export world` and the contents of a component

### 13.5.1 The export world

- The root `sai.package` may name one world with `export world` (syntax: [package/01](../package/01-package-contract.md)). The name is either a module path of a local world relative to the root module (`client`, `api/client`) or the `PKG_PATH` of an external world (for example `wasi:http/proxy`). A name that resolves to no world is `UNKNOWN_EXPORT_WORLD`.
- For an external world, the package must implement every interface and function that the world exports (§13.4.1); the world's imports are available to it like those of a local world.
- Only the root package's `export world` determines the exports of a build and bounds its imports. The `export world` of a dependency is used when that dependency is itself built as the root, when its prebuilt artifact is used, and when it is a hybrid dependency used in source form ([package/02](../package/02-resolver.md), [package/04](../package/04-distribution-and-security.md) §4.5.1, D-PKG-05). In source form the hybrid is compiled into the root's module with its `export world` applied, and it **provides within the build** the interfaces that world exports (N-D-104): the calls of its dependents, which reach it through the SAI view of its WIT in both forms (package/04 §4.5.3), are linked to its interface impls across a Canonical ABI boundary inside the module. Its world adds no export to the component, and the interfaces it provides are not imports of the component (§13.5.2).
- The export world is the **contract** of the component: it bounds what the component may import and fixes what it exports.

### 13.5.2 Exports and imports of a component

Let `W'` be the elaborated export world (§13.4.2), extended for commands as in §13.5.3. The component produced by a component build has:

- **Exports:** exactly the exported interfaces and functions of `W'`. No other export is visible in the component type; core-level exports such as memory, `cabi_realloc` and post-return functions are internal to the component ([impl/07](../impl/07-component-lowering.md)).
- **Imports:** exactly the set `Imp` of interfaces (and the world-import function block, if any) such that one of the following holds:
  1. an interface effect of the interface is in `Own(f)` of a reachable body `f` ([08 §8.12.3](08-effects-and-capabilities.md));
  2. a type of the interface occurs in a boundary type of an export of `W'` (N-D-14 of [08](08-effects-and-capabilities.md));
  3. a type of the interface occurs in a boundary type of an interface already in `Imp`, transitively.

  Unused imports of `W'` are omitted (draft §39). A component with fewer imports than its world is a valid implementation of that world. Interfaces that the build provides itself are removed from these imports, and the imports of composed artifacts are added, as described below.

Every interface in `Imp` that the build does not provide (below) MUST be imported by `W'`. An interface in `Imp` that `W'` does not import is the error `WORLD_IMPORT_UNDECLARED`, reported with the why chain of [08 §8.13](08-effects-and-capabilities.md) and a structured fix that adds `import <interface>` to the world declaration if the world is local. In addition, every interface in `Imp` is a required capability and is checked against the policies ([08 §8.12.6](08-effects-and-capabilities.md)).

**Provided interfaces.** An interface of `Imp` is **provided** when the build itself contains its implementation. There are two providers (N-D-101, N-D-104):

- **By composition.** A package in artifact form ([package/04](../package/04-distribution-and-security.md) §4.5.1) whose artifact exports the interface. The component is then the composition of the root's component with the artifacts that provide its imports ([impl/07](../impl/07-component-lowering.md) §7.11.9). An import of a composed artifact that no other member of the composition satisfies is an import of the component.
- **Within the build.** A hybrid dependency in source form whose `export world` exports the interface (§13.5.1). Its impls are compiled into the root's core module, and each call of a function of the interface is linked to the impl through the Canonical ABI glue of both sides, inside the module ([impl/07](../impl/07-component-lowering.md) §7.11.10). This applies on every target, including core and native targets.

The provider of an interface `ns:pkg/iface@v` is the active node with identity `ns:pkg` and selected version `v`, so that each compatibility line (§13.7.1) has its own provider and each provider is matched with the view of its own version. Interfaces of the `wasi` namespace are never provided, and only interfaces are provided, never world functions. A package without an `export world` never provides: an interface of another package is always reached as an imported function (N-D-29), and only the provider differs. The exports are those of `W'` in every case, and what a provider exports is never visible (package/04 §4.5.2 item 3). The rule above then applies to the imports that remain (N-D-101, N-D-106):

- a provided interface is not an import of the component and need not be imported by `W'`;
- an import of a composed artifact that remains an import of the component MUST be imported by `W'`; otherwise it is `WORLD_IMPORT_UNDECLARED`, explained by the artifact that imports it;
- the code of a provider within the build is code of the build: an interface that it calls from a reachable body is in `Imp` by rule 1 and is subject to the rule above, explained by the why chain through the provided call.

A call of a provided function has the semantics of a component call in every form (N-D-104). Arguments and results are copied. The types of the SAI view and the provider's own types are distinct nominal types, and the provider's trait impls are not visible to its callers. The call has the interface effect of the interface ([08 §8.12.3](08-effects-and-capabilities.md)), because effect analysis does not depend on the form. A trap or panic in the provider ends the whole instance, as a trap in a composed artifact ends the composed component, and no destructors run (§13.10.4). The async items that §13.9.2 rejects in imported interfaces are rejected in the same way in provided ones.

A provided interface MUST NOT cross the boundary of the component (N-D-105): `W'` MUST NOT export it; no type of it MAY occur in a boundary type of an export of `W'` or of an interface that remains an import of the component; and no composed artifact MAY import an interface that is provided within the build. Each violation is `PROVIDED_INTERFACE_AT_BOUNDARY`, reported once per interface and case ([package/04](../package/04-distribution-and-security.md) §4.5.3 item 5).

Providing changes no required capability: every interface in `Imp`, and every import of every artifact in use, is a required capability whether or not the build provides it ([08 §8.12.4](08-effects-and-capabilities.md)).

World imports never grant capabilities (D-WIT-03): a world import is permission at the level of the component contract, and `allow` is permission at the level of the package policy. Both checks apply independently.

### 13.5.3 Commands and reactors

A component is a **command** if the root module defines the entry point `pub fn main(env: owned process.Env) -> process.ExitCode` (D-CAP-02, [16-core-library.md](16-core-library.md)); otherwise it is a **reactor** (N-D-31).

- **Command.** `W'` is the export world extended as if it contained `include wasi:cli/command` at the WASI version of the target: it exports `wasi:cli/run` and may import every interface of the `wasi:cli/imports` world. The compiler generates the implementation of `run` ([impl/07](../impl/07-component-lowering.md)): it constructs the `process.Env` value, calls `main`, drops the result's resources, and returns `ok` if the `ExitCode` is success (zero) and `err` otherwise. If no `export world` is declared, `W'` is exactly this extension of an empty world. If the export world itself exports `wasi:cli/run` while `main` exists, the duplicate is `WORLD_DUPLICATE_ITEM`.
- **Reactor.** `W'` is the elaborated export world. The component has no entry point; the host calls its exports. SAI has no static initializers (D-CAP-02), so a reactor has no user-visible initialization step.
- A component build of a reactor without an `export world`, or whose `W'` has no exports, is `NO_COMPONENT_EXPORTS`. `sai check` does not report it; it is reported only when a component artifact is produced.
- The `artifact` declaration of `sai.package` ([package/01](../package/01-package-contract.md)) selects which artifacts a build produces; it cannot change whether a component is a command or a reactor.

---

## 13.6 Worlds and targets

### 13.6.1 Which builds produce components

| Build | Output | Export world | WASI |
|---|---|---|---|
| `sai build --target wasm32-wasi` / `wasm64-wasi` | component | applied (§13.5) | the WASI 0.2 packages of the targets doc |
| `sai component build --target wasm32-wasi` / `wasm64-wasi` | component | applied | as above |
| `sai build --target wasm32-core` / `wasm64-core` | core module | **not applied** | none |
| `sai component build --target wasm32-core` / `wasm64-core` | component wrapping the core module | applied, exports only | none |
| `sai build` for a native target | native executable | **not applied** | linked into the executable |
| `sai component build` for a native target | none: `COMPONENT_TARGET_MISMATCH` | not applicable | not applicable |

`wasm64-wasi` is experimental because it depends on memory64 support in the Component Model (D-TGT-01).

A native target ([targets](../targets/targets-and-profiles.md) §2.8) produces an executable, never a component. `sai build` for a native target does not apply the `export world` and reports `EXPORT_WORLD_NOT_APPLIED` as in §13.6.2 item 1, and it links the toolchain's own implementation of the WASI interfaces instead of importing them (targets §2.8.4). `sai component build` for a native target is the error `COMPONENT_TARGET_MISMATCH`, because a native executable cannot be a component (targets §2.8.5 item 1); no artifact is produced.

### 13.6.2 Core targets

A core module cannot import or export WIT interfaces. The rules for core targets are (N-D-32):

1. `sai build` for a core target produces a core module whose exports are exactly the `export fn` items of the root package (D-MOD-02, D-WASM-04). The `export world` is not applied: its exports are not exported, its imports are not imported, and interface impls are compiled only if reachable from an `export fn`. The `export world` of a hybrid dependency in source form is still applied, so that the dependency provides its interfaces within the build (§13.5.2). If the root declares `export world`, the build emits the information diagnostic `EXPORT_WORLD_NOT_APPLIED` once, suggesting `sai component build`. Hybrid packages that target both core and WASI targets therefore build on both without errors.
2. `sai component build` for a core target wraps the core module as a component whose exports are those of the elaborated export world, and which has **no imports** (D-TGT-01: WIT exports only, no WASI). Every interface in `Imp` (§13.5.2) that is not provided within the build is the error `TARGET_CAPABILITY_UNAVAILABLE` ([08 §8.12.7](08-effects-and-capabilities.md)), including interfaces needed only for the types of exported signatures. An interface provided within the build is available, because no import is needed for it (N-D-104). World imports that are unreachable are permitted and omitted.
3. A component built for a core target is always a reactor. If the root module defines `main`, the build is `COMPONENT_TARGET_MISMATCH`, because the command entry point needs `wasi:cli`.
4. On a core target, any reachable call of an imported interface function is `TARGET_CAPABILITY_UNAVAILABLE` in every build, whether or not a component is produced. A function of an interface provided within the build (§13.5.2) is not imported: its call is linked inside the module and is available on every target. The interfaces that the provider itself calls are subject to this rule.

### 13.6.3 Exporting WASI interfaces

A package exports a WASI interface by implementing its binding. For example, an HTTP handler for the `wasi:http/proxy` world:

```sai
use wasi:http/incoming-handler
use wasi:http/types.{Fields, IncomingRequest, OutgoingResponse, ResponseOutparam}

impl incoming_handler {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam)
        effects { net.http }
    {
        let _ = request
        let response = OutgoingResponse.new(Fields.new())
        ResponseOutparam.set(response_out, Ok(response))
    }
}
```

with `export world wasi:http/proxy` in `sai.package`. The resources `IncomingRequest` and `ResponseOutparam` are owned handles here, because the WIT signature `handle: func(request: incoming-request, response-out: response-outparam)` passes `own` handles. `OutgoingResponse.new` and `Fields.new` are constructor calls of imported resources (§13.7.4); `ResponseOutparam.set` is a static function. All three have the interface effect `types`, permitted by `net.http` ([08 §8.2.4](08-effects-and-capabilities.md)). This code also triggers `AMBIENT_IMPORT` (D-CAP-02).


### 13.6.4 Core-level items in component builds

A component exports only the items of `W'` and imports only WIT interfaces (§13.5.2). Two kinds of core-level items therefore do not carry over into a component build, whether the target is a WASI target or a core target wrapped by `sai component build` (N-D-46):

- **`export fn` items** (D-MOD-02) are not exported from the component. They are compiled only if reachable from another root, and they are not roots of reachability in a component build ([08 §8.12.3](08-effects-and-capabilities.md)). If the root package declares at least one `export fn`, the build emits the information diagnostic `EXPORT_FN_NOT_APPLIED` once, so that hybrid packages build on both kinds of target.
- **`extern "wasm"` functions** (D-UNSAFE-01) cannot be satisfied by a component. A component artifact does not provide the target capability `host`, so a reachable call of an `extern "wasm"` function is `TARGET_CAPABILITY_UNAVAILABLE` ([08 §8.12.7](08-effects-and-capabilities.md)).

---

## 13.7 Importing WIT into SAI

### 13.7.1 Where WIT comes from

The compiler reads WIT directly (D-WIT-02). The WIT packages visible to a build are:

1. the WASI 0.2 packages shipped with the toolchain, at the WASI version that the target fixes ([targets doc](../targets/targets-and-profiles.md)): `wasi:cli`, `wasi:clocks`, `wasi:filesystem`, `wasi:http`, `wasi:io`, `wasi:random` and `wasi:sockets`;
2. the `wit/` directory of every Wasm package in the resolved dependency graph (D-WIT-02);
3. WIT packages declared by the root with the `wit` declaration of `sai.package` ([package/01](../package/01-package-contract.md));
4. the WIT generated (§13.11) from the interfaces and worlds of every SAI source package in the graph.

A `wit/` directory is read without its `deps/` subdirectory, if any. A foreign WIT package that its files refer to MUST be one of the packages above, reached through the package graph (P1: dependencies are declared in `sai.package` only).

A WASI package has exactly one version in a build, the one that the target fixes. A target that fixes no WASI version, such as a core-module target, uses the WASI version of the WASI targets of the same toolchain release ([targets doc](../targets/targets-and-profiles.md) §2.5 item 1), so a program is checked against the same WASI on every target. The WIT package of a package in the graph, whose name is the package identity ([package/01](../package/01-package-contract.md) §1.6.3 item 3), has the version that resolution selects for its node ([package/02](../package/02-resolver.md)), recorded with its WIT checksum in `sai.lock` (D-PKG-03). A graph has one node per identity and compatibility line (D-PKG-02). When it contains two lines of one package ([package/02](../package/02-resolver.md) §2.9), the WIT package of that name is visible in two versions, one per line: a `use` or world item of a package names the version of the node that its own edge selects (package/02 §2.9 item 5), and a component that imports from both lines has both versions in its import names (§13.11.4). The identity of an interface effect and a capability name are versionless ([08](08-effects-and-capabilities.md) N-D-1, [package/01](../package/01-package-contract.md) §1.11.2 item 3), so an interface of either line has the same effect atom and is covered by the same capability names. WIT that fails to parse or validate is `WIT_PARSE_ERROR`, reported with the WIT file and position.

### 13.7.2 Binding with `use`

- `use ns:name/iface` binds the WIT interface `iface` of package `ns:name` in the current module under the name obtained by the conversion of §13.8 (D-MOD-01): `use wasi:http/outgoing-handler` binds `outgoing_handler`.
- `use ns:name/iface as alias` binds it as `alias`. `use ns:name/iface.{Item, func as g}` binds members ([04-modules-and-names.md](04-modules-and-names.md)); member names are written in their SAI form (`.{OutgoingRequest}`, not `.{outgoing-request}`).
- `use` never binds a world: `use ns:name/world` is `INVALID_USE_TARGET` ([04 §4.3.2](04-modules-and-names.md) step 3.2). A world of a WIT package is named by its `PKG_PATH` where a world is expected, in `include` (§13.4.1) and as an `export world` (§13.5.1).
- The path segment after `/` is the WIT name of the interface or world, written in lowercase kebab-case. A WIT name containing uppercase words is written in lowercase (Component Model names are unique case-insensitively). Further `/` segments are not allowed for WIT packages. A path that names no interface or world of the WIT package is `UNKNOWN_WIT_ITEM`.
- No version is written in source (the grammar has none, [03 §3.3](03-grammar.md)); the version comes from resolution (N-D-33).

### 13.7.3 The SAI view

Every visible WIT interface has a SAI view, a set of SAI declarations computed deterministically from the WIT (D-WIT-02):

| WIT item | SAI view |
|---|---|
| `record r { … }` | `type R { fields }`, fields in WIT order |
| `variant v { … }` | `enum V { … }` with one payload type per case |
| `enum e { … }` | `enum E { … }` with unit variants |
| `flags f { … }` | `@flags enum F { … }` |
| `resource r { … }` | an opaque resource type `R` with its members (§13.7.4) |
| `type a = t;` | `type A = T` |
| `use other.{t};` / `use other.{t as u};` | `type T = other.T` / `type U = other.T` |
| `f: func(…) -> …;` | `fn f(…) -> …` |

- Names are converted by §13.8 and types by §13.9.2.
- WIT doc comments become doc comments of the view.
- WIT `@deprecated` becomes SAI `@deprecated` ([15-attributes-and-lints.md](15-attributes-and-lints.md)). WIT `@since` has no effect, because the version is fixed by resolution. Items marked WIT `@unstable(feature = …)` are not part of the view in v0.1 (N-D-34).
- The view derives traits structurally (N-D-35): enums and flags implement `Copy Clone PartialEq Eq Hash`; a record or variant implements `Clone` and `PartialEq` if every field or payload type does, and `Copy` and `Eq` if every field or payload type does. Resources implement none of them (D-OWN-06).
- The nominal identity of a view type is its WIT identity `ns:name/iface#name`; two bindings of the same interface denote the same types.

### 13.7.4 Imported resources

For an imported WIT resource `r`:

- `R` is an affine, opaque resource type with no fields. Values of `R` are owned handles; they can be created only by calling the resource's constructor, static functions or other imported functions. Using record-construction syntax with an imported resource type is `OPAQUE_RESOURCE_CONSTRUCTION`.
- `constructor(…)` becomes `fn new(…) -> R`; a WIT constructor whose result is `result<r, e>` becomes `fn new(…) -> Result<R, E>`.
- `name: func(…)` inside the resource (a method) becomes `fn name(self: borrow Self, …)`.
- `name: static func(…)` becomes an associated function `fn name(…)` without self.
- A static function named `new` in a resource that also has a constructor collides with the constructor (warning `WIT_ITEM_SHADOWED`, §13.8.5); the static function is not accessible from SAI.
- Members are called as `R.new(…)`, `value.method(…)` (method-call resolution as in [05-types.md](05-types.md)), and `R.name(…)`; through the interface binding they are `iface.R.new(…)` and so on.
- Dropping an owned handle calls the Component Model `resource.drop` for that handle (D-OWN-06, §13.10.4).

---

## 13.8 Name conversion

### 13.8.1 WIT names

A WIT name (label) is `word ('-' word)*` with `word ::= [a-z][a-z0-9]* | [A-Z][A-Z0-9]*`. Within one WIT scope, names are unique case-insensitively. A WIT name that is a WIT keyword is written with a leading `%` in WIT text (`%type`); the `%` is not part of the name.

### 13.8.2 Which SAI case

| WIT item | SAI identifier kind (D-LEX-02) |
|---|---|
| interface, world, function, parameter, record field | lower, `snake_case` |
| record, variant, enum, flags, resource, type alias | upper, `UpperCamelCase` |
| variant case, enum case, flag | upper, `UpperCamelCase` |
| package namespace and name | not converted: they are written as `PKG_PATH` segments |

### 13.8.3 WIT to SAI

Given a WIT name with words `w1 … wn`:

1. Lowercase every word that is all-uppercase (`HTTP` → `http`).
2. For a lower identifier: join the words with `_` (`outgoing-handler` → `outgoing_handler`, `ipv4-address` → `ipv4_address`).
3. For an upper identifier: capitalize the first letter of each word and concatenate (`incoming-request` → `IncomingRequest`, `HTTP-error` → `HttpError`, `ipv4-address` → `Ipv4Address`).
4. If the result is a strict keyword or a reserved word (D-LEX-04), prefix it with `r#` (`type` → `r#type`, `loop` → `r#loop`, `self` → `r#self`). Contextual keywords are not escaped, because they are valid identifiers.

Because WIT names are unique case-insensitively, steps 1–3 are injective within each WIT scope and within the item kinds of §13.8.2.

### 13.8.4 SAI to WIT

Given a SAI identifier, first strip a `r#` prefix. Then:

- **Lower identifier.** It MUST match `[a-z][a-z0-9]*(_[a-z][a-z0-9]*)*`: ASCII, no uppercase letters, no leading, trailing or doubled `_`, and every `_`-separated segment starts with a letter. Replace each `_` by `-` (`fetch_user` → `fetch-user`, `sha256_sum` → `sha256-sum`).
- **Upper identifier.** It MUST match `([A-Z][a-z0-9]*)+`: ASCII letters and digits only, no `_`. Split it before every uppercase letter, lowercase each part, and join with `-` (`ImageError` → `image-error`, `Ipv4Address` → `ipv4-address`, `TooLarge` → `too-large`, `HTTPClient` → `h-t-t-p-client`).

An identifier that does not match is the error `WIT_NAME_NOT_REPRESENTABLE`, with a suggested representable name (`utf_8` → suggest `utf8`; `User_Id` → suggest `UserId`). If the resulting WIT name is a WIT keyword, the generated WIT writes it with `%` (`r#type` → `%type`).

The two directions are inverse on every name that can cross the boundary: for every representable SAI identifier `s`, converting `s` to WIT and back yields `s` (modulo `r#`, which step 4 of §13.8.3 restores exactly when needed). For every WIT name `w`, converting to SAI and back yields a name equal to `w` case-insensitively (N-D-36).

### 13.8.5 Collisions

The SAI-to-WIT conversion MUST be injective within each WIT scope (D-WIT-01):

- the interfaces and worlds of one package, across all of its modules (one WIT package namespace: interfaces `a.net` and `b.net` declared in modules `a` and `b` collide);
- the types and functions of one interface (one WIT namespace, although SAI separates them by case: a function `image` and a resource `Image` collide);
- the imports of a world, and the exports of a world;
- the members of one resource (the constructor is a separate name);
- the fields of a record; the cases of a variant, enum or flags; the parameters of a function.

Two SAI items that map to the same WIT name in one scope are the error `WIT_NAME_COLLISION`, reported at the later item in source order; across modules, the order is that of §13.11.1. In the import direction, a collision can arise only from the `new` rule of §13.7.4; it is reported as the warning `WIT_ITEM_SHADOWED` (the WIT cannot be changed by the user, so it is not an error) and the later item is not accessible.

### 13.8.6 Examples

| SAI | WIT |
|---|---|
| interface `outgoing_handler` | `outgoing-handler` |
| function `fetch_user` | `fetch-user` |
| record `UserProfile` | `user-profile` |
| variant case `NotFound` | `not-found` |
| field `r#type` | `%type` |
| resource `Ipv4Socket` | `ipv4-socket` |
| world `image_service` | `image-service` |
| function `get2` | `get2` |
| function `_hidden` | rejected: `WIT_NAME_NOT_REPRESENTABLE` |
| function `to_v_2` | rejected: segment `2` starts with a digit |

---

## 13.9 Type mapping

Positions used below: **P** is the top-level type of a function parameter, **R** the return type of a function, and **N** any other position (record field, variant payload, element of a list, option, result, tuple, alias target). "Any" means P, R and N.

### 13.9.1 SAI to WIT

This table applies to local interfaces and worlds, and to the impls of external interfaces (whose SAI view, §13.9.2, is already in this form).

| SAI type | WIT type | Positions | Notes |
|---|---|---|---|
| `Bool` | `bool` | any | |
| `I8` `I16` `I32` `I64` | `s8` `s16` `s32` `s64` | any | `Int` is `I64` (D-TYPE-01) |
| `U8` `U16` `U32` `U64` | `u8` `u16` `u32` `u64` | any | `UInt` is `U64` |
| `Byte` | `u8` | any | |
| `F32` `F64` | `f32` `f64` | any | NaN payloads are not preserved across the boundary |
| `Char` | `char` | any | a Unicode scalar value on both sides |
| `String` | `string` | any | |
| `Str` | `string` | P | borrowed for the duration of the call |
| `Vec<T>` | `list<T'>` | any | |
| `Bytes` (`Vec<Byte>`) | `list<u8>` | any | R-08 |
| `Slice<T>` | `list<T'>` | P | `T` must not contain a resource type |
| `Option<T>` | `option<T'>` | any | `T` must not be `()` |
| `Result<T, E>` | `result<T', E'>` | any | `Result<(), E>` → `result<_, E'>`, `Result<T, ()>` → `result<T'>`, `Result<(), ()>` → `result` |
| `(T1, …, Tn)`, n ≥ 1 | `tuple<T1', …, Tn'>` | any | |
| `()` | no result | R only | also as an arm of `Result` (above) |
| `owned T` | as `T` | as `T` | the `owned` marker does not change the WIT type |
| interface record / alias / enum / variant / flags `X` | its WIT name `x` | any | declared in the same interface or in another interface (`other.X`, WIT `use`) |
| interface resource `R` (an owned value) | `own<r>`, written `r` | any | |
| `borrow R`, `borrow mut R` (`R` a resource) | `borrow<r>` | P | `borrow Self` / `borrow mut Self` as the method receiver |
| `FutureReader<T>` (`sai:async`) | `future<T'>` | any | Stage 5; the readable end ([11](11-async-and-concurrency.md) §11.10.3 item 3). `Future` is a trait, not a type (D-ASYNC-01), so it has no mapping |
| `StreamReader<T>` (`sai:async`) | `stream<T'>` | any | Stage 5; the readable end (11 §11.10.3 item 3). `Stream` is a trait and has no mapping |

`T'` denotes the WIT mapping of `T`.

### 13.9.2 WIT to SAI (the SAI view)

| WIT type | SAI type in a top-level parameter of a view function | SAI type elsewhere |
|---|---|---|
| `bool` | `Bool` | `Bool` |
| `s8` … `s64` | `I8` … `I64` | same |
| `u8` … `u64` | `U8` … `U64` | same |
| `f32` `f64` | `F32` `F64` | same |
| `char` | `Char` | `Char` |
| `string` | `Str` | `String` |
| `list<u8>` | `Slice<Byte>` | `Bytes` |
| `list<T>`, `T` contains no handle | `Slice<T>` | `Vec<T>` |
| `list<T>`, `T` contains a handle | `Vec<T>` | `Vec<T>` |
| `option<T>` | `Option<T>` | same |
| `result<T, E>` / `result<_, E>` / `result<T>` / `result` | `Result<T, E>` / `Result<(), E>` / `Result<T, ()>` / `Result<(), ()>` | same |
| `tuple<…>` | `(…)` | same |
| record, variant, enum, flags, resource, alias | the view type (§13.7.3) | same |
| `own<r>` / `r` | `R` | `R` |
| `borrow<r>` | `borrow R` | (WIT allows it only in parameters) |
| `future<T>` / `stream<T>` | `FutureReader<T>` / `StreamReader<T>` of `sai:async` (Stage 5, [11](11-async-and-concurrency.md) §11.10.3 item 3) | same |
| `error-context`, fixed-length `list<T, N>`, `map<K, V>`, multiple named results | not supported: `WIT_UNSUPPORTED_FEATURE` | |

- A function without a result has the SAI return type `()`.
- The view of the parameters uses borrowed `Str` and `Slice` so that calling an import never forces a copy into an owned value (N-D-37). The view is used both for calling imports and for implementing external interfaces (§13.3.2).
- `WIT_UNSUPPORTED_FEATURE` is reported when a SAI program *uses* an item whose signature contains an unsupported construct, not when the WIT package is loaded. Other items of the same interface remain usable.
- A WIT world that declares types or `use` statements of its own, or that contains an inline `interface { … }`, has no SAI form: SAI worlds declare no types (§13.4.1), and an inline interface has no `ns:pkg/name` by which SAI could name it. Such a world is unsupported, and an `include` of it or an `export world` that names it is `WIT_UNSUPPORTED_FEATURE`, reported at the reference.
- **Imported async items.** A WIT function of an imported package whose signature is `async` or contains `future<T>` or `stream<T>` needs the Component Model async ABI, which no v0.1 target provides ([targets](../targets/targets-and-profiles.md) §3.4). Before Stage 5 its use is `FEATURE_NOT_IMPLEMENTED` (§13.2.2); from Stage 5 on, on a target without the async ABI, its use is `WIT_UNSUPPORTED_FEATURE` (targets §3.4 item 2). As for the constructs of the table, the diagnostic is reported where a SAI program uses the item, not when the WIT package is loaded. The stable WASI 0.2.3 subset has no such item, so no WASI import is affected.

### 13.9.3 The WIT-compatibility rule (R-08)

A boundary type `T` in position `p` is **WIT-compatible** when it is admitted by §13.9.1 in position `p`, recursively for its components with position N. In particular, the following are never WIT-compatible (D-WIT-01):

- `ISize`, `USize`;
- function pointer types, function item types, closure types and type parameters;
- `Never`;
- `Array<T, N>` (WIT fixed-length lists are not available in WASI 0.2, v0.1);
- `Box<T>` and arena references;
- `borrow T` and `borrow mut T` where `T` is not a resource, in any position (use the owned type, or `Str` / `Slice<T>` in P);
- `Str`, `Slice<T>`, `borrow R` in R or N positions (D-WIT-01: no non-resource return type containing borrows);
- refined types and distinct types;
- every nominal type (struct, enum, resource, distinct) declared outside an interface (R-08). The help suggests declaring the type in the interface.

A non-compatible boundary type is `WIT_INCOMPATIBLE_TYPE`, reported at the type in the interface or world declaration, with the reason and the position. The check applies to interface functions, resource members, world functions, record fields, variant payloads and alias targets.

Invariants (`invariant R { … }`) may be declared for interface records in the module that contains the interface (`INVARIANT_OUTSIDE_DEFINING_MODULE` otherwise). They are not part of WIT; values received by an export are validated against them at run time (D-CONTRACT-01, [09-contracts-and-refinements.md](09-contracts-and-refinements.md)).

### 13.9.4 Flags

`@flags` (D-SYN-09) on an enum inside an interface declares a **flags type** (N-D-38):

```sai
interface storage {
    @flags
    enum Perms {
        Read
        Write
        Exec
    }
    fn chmod(path: Str, perms: Perms) -> Result<(), ()>
}
```

- Every variant MUST be a unit variant without discriminant, and there MUST be at most 32 variants (`INVALID_FLAGS_ENUM`). `@flags` outside an interface is `INVALID_FLAGS_ENUM` in v0.1. A flags enum without variants is `WIT_EMPTY_TYPE` (§13.2.2), like every empty interface type. `INVALID_FLAGS_ENUM` is reported at most once per enum, for the first rule that it breaks in this order: outside an interface, a payload, a discriminant, more than 32 variants. It replaces `WIT_EXPLICIT_DISCRIMINANT` for that enum.
- A value of a flags type is a **set** of its flags, not one variant. Each variant name `Perms.Read` is a constant of type `Perms` containing exactly that flag. The type is `Copy` and implements `PartialEq`, `Eq`, `Hash`, `BitOr`, `BitAnd` and `BitXor` (union, intersection, symmetric difference), and has the associated functions `empty() -> Self` and `all() -> Self` and the methods `contains(self: Self, other: Self) -> Bool` and `is_empty(self: Self) -> Bool`.
- A flags type cannot be matched with variant patterns; a `match` whose patterns name its flags is `FLAGS_PATTERN`. Use `contains` or `==` instead.
- The view of an imported WIT `flags` type has the same form.

```sai
fn writable(p: storage.Perms) -> Bool {
    return p.contains(storage.Perms.Write)
}

fn read_write() -> storage.Perms {
    return storage.Perms.Read | storage.Perms.Write
}
```

---

## 13.10 Resources and handles

### 13.10.1 Affine semantics

A resource value is affine (D-OWN-06): it is moved, never copied, and it is dropped exactly once unless it is moved away first ([07-ownership-and-borrowing.md](07-ownership-and-borrowing.md)). At the boundary, a SAI value of resource type is always an **owned handle** (`own<r>`), and `borrow R` is a **borrowed handle** (`borrow<r>`). These are the only two forms; WIT has no mutable borrow, and `borrow mut R` maps to `borrow<r>`.

### 13.10.2 Handles crossing the boundary

| Operation | Effect on ownership |
|---|---|
| passing `R` to an imported function | ownership moves to the callee; the SAI value is consumed and not dropped |
| receiving `R` from an imported function | the caller owns a new handle |
| passing `borrow R` to an imported function | the handle is lent for the duration of the call; the borrow checker keeps the owner alive |
| an export returning a bound concrete resource `C` (as `iface.R`) | the value moves into the component's resource table; the client receives a new owned handle |
| an export receiving an owned handle of its own resource | the value moves out of the resource table into the SAI parameter |
| an export receiving a borrowed handle of its own resource | the parameter is `borrow C` (or `borrow mut C`) to the value in the table, valid only during the call |
| an export receiving an owned or borrowed handle of an imported resource | the parameter is an owned `R` or a `borrow R` of the imported resource |
| `Vec<R>`, `Option<R>` and other aggregates containing `R` | each contained handle is transferred as above |

- A borrowed handle received by an export cannot outlive the call: its region is the call ([07-ownership-and-borrowing.md](07-ownership-and-borrowing.md)), so it cannot be stored or returned. The glue releases it before the export returns ([impl/07](../impl/07-component-lowering.md)).
- `borrow mut Self` methods of exported resources are sound because the Component Model does not let a client re-enter an instance while one of its exports is running; the mutable borrow of the value in the table is therefore unique.

### 13.10.3 Invalid handles

The Component Model validates every handle crossing the boundary. Lifting an index that is not a live handle of the expected resource type, dropping a resource while it is lent, or failing to release a borrowed handle traps ([impl/07](../impl/07-component-lowering.md)). SAI code cannot produce such handles: handle indices are never visible as integers.

### 13.10.4 Drop

- Dropping an owned handle of an **imported** resource calls the canonical `resource.drop`; the exporter may run its destructor. This has no effect in the sense of [08](08-effects-and-capabilities.md) (N-D-4 there).
- When the client of an **exported** resource drops its last owned handle, the component's destructor runs: it drops the concrete value, running its `Drop` impl (pure, D-OWN-05) and the drop glue of its fields.
- Borrowed handles are never dropped by SAI code; releasing them is glue.
- When an instance is torn down (for example after a trap), no destructors run.

---

## 13.11 Generated WIT

### 13.11.1 What is generated

`sai bindgen wit` (D-WIT-02) and `sai inspect wit` (D-TOOL-03) produce the WIT package of the current SAI package. The command-line interface and output paths are defined in [impl/13](../impl/13-cli.md); the content is defined here. The WIT embedded in a component ([impl/07](../impl/07-component-lowering.md)) is semantically identical to this text.

The generated WIT package contains:

- every interface declared in the package, regardless of SAI visibility;
- every world declared in the package, **as written** (not elaborated, §13.4.2, and not pruned, §13.5.2).

Interfaces and worlds are ordered by the module path of their declaring module (root module first, then byte-wise lexicographic order of module paths), then by source order within the module. All interfaces come before all worlds.

### 13.11.2 Text format

The text is exactly determined by the following rules (N-D-39):

- UTF-8, LF line endings, no BOM, no trailing whitespace, no tab characters, and exactly one LF at the end of the file.
- Indentation is two spaces per level.
- The first line is `package ns:name@version;`, where `ns:name` and `version` come from the `package` declaration of `sai.package` (D-PKG-01, draft §42). The version is the full semantic version, including pre-release and build metadata if present.
- Exactly one blank line separates top-level items, and the package line from the first item.
- Inside an interface, the `use` statements come first (§13.11.3), followed by one blank line if there are any, followed by the items in source order with exactly one blank line between items.
- Inside a world, generated `use` statements come first (§13.11.3), followed by one blank line if there are any, followed by the world items in source order, one per line without blank lines.
- Each doc-comment line of a SAI item is printed as `/// text` on its own line directly above the item, at the item's indentation. Inner doc comments are not printed.
- Names are converted by §13.8.4 and written with `%` if they are WIT keywords.

| SAI item | Printed form |
|---|---|
| record | `record name {`, then one line per field `field: type,`, then `}` |
| alias | `type name = type;` |
| enum | `enum name {`, then one line per case `case,`, then `}` |
| variant | `variant name {`, then one line per case `case,` or `case(type),`, then `}` |
| flags | `flags name {`, then one line per flag `flag,`, then `}` |
| resource without members | `resource name;` |
| resource with members | `resource name {`, then the constructor (if any) as `constructor(params);`, then methods as `name: func(params) -> result;` and statics as `name: static func(params) -> result;` in source order, then `}` |
| function | `name: func(params) -> result;` on a single line; without a result: `name: func(params);` |
| world import / export of an interface | `import iface;` / `export iface;` (local) or `import ns:pkg/iface@version;` (external) |
| world function | `import name: func(params) -> result;` / `export name: func(params) -> result;` |
| world include | `include world;` (local) or `include ns:pkg/world@version;` (external) |

- Parameters are printed `name: type` separated by `, `. The `self` parameter of a method is not printed.
- Type syntax: `list<t>`, `option<t>`, `result<t, e>`, `result<_, e>`, `result<t>`, `result`, `tuple<a, b>`, `borrow<r>`, `future<t>`, `stream<t>`; an owned handle is printed as the bare resource name `r`.
- No line is ever wrapped, regardless of length.

### 13.11.3 Cross-interface references

For each interface `i`, the generated `use` statements are computed from the boundary types of `i`. For each world `w`, they are computed from the parameter and result types of its direct world functions (imports and exports), recursively through type constructors:

- one statement per referenced interface, `use other.{a, b};` for a local interface and `use ns:pkg/other@version.{a, b};` for an external one;
- statements sorted byte-wise by the text between `use ` and `.{`; names inside the braces sorted byte-wise.
- world references are collected only from direct function items, not from included worlds; each statement appears at the start of the world before its direct items, with one blank line after the final statement.
- if a type spelling in one world refers to different interface items, or a generated type alias conflicts with a world import function name, the affected generated function item is omitted and the generated text is incomplete. An imported interface key is distinct from a world-name key and does not by itself conflict with a type alias.

A type of another interface is then printed by its plain WIT name. A generated world uses the same rule; its function signatures refer to the aliases in that world's leading `use` statements.

### 13.11.4 Versions

- The version of the generated package is the SAI package version (draft §42: package identity is aligned with WIT).
- Each external reference carries the version selected by resolution (§13.7.1); WASI references carry the WASI version of the target. The generated WIT therefore depends on the resolved graph and on the target's WASI version, and is regenerated when either changes.
- Changing any part of the generated WIT is an API change of the package; semantic diff reports it ([impl/12](../impl/12-semantic-tooling.md)).

### 13.11.5 Example

The package `acme:imaging@0.3.0` containing the declarations of §13.3.3, with a doc comment `/// Scales an image.` on `resize` and the WASI version 0.2.3, generates:

```wit
package acme:imaging@0.3.0;

interface processor {
  enum image-error {
    too-large,
    unsupported,
  }

  resource image {
    constructor(data: list<u8>);
    width: func() -> u32;
  }

  /// Scales an image.
  resize: func(image: list<u8>, width: u32, height: u32) -> result<list<u8>, image-error>;
}

world image-service {
  import wasi:http/outgoing-handler@0.2.3;
  export processor;
}
```

The `users` interface and `client` world of R-04 generate the following (the package line is omitted):

```wit
interface users {
  enum user-error {
    not-found,
    network,
    invalid-response,
  }

  record user {
    id: u64,
    name: string,
  }

  fetch-user: func(id: u64) -> result<user, user-error>;
}

world client {
  import wasi:http/outgoing-handler@0.2.3;
  export users;
}
```

### 13.11.6 Round trip

For every local interface `I`, the SAI view (§13.7.3) of the generated WIT of `I` is equal to `I` after the following normalizations: aliases are kept, `Str` in parameters stays `Str`, `String` in parameters becomes `Str`, `Vec<T>` and `Bytes` in parameters become `Slice<T>` and `Slice<Byte>` when `T` contains no handle, `borrow mut R` becomes `borrow R`, and `owned T` becomes `T`. Tools may rely on this (impl/12).

---

## 13.12 Effects and capabilities at the boundary

The rules are defined in [08-effects-and-capabilities.md](08-effects-and-capabilities.md); this section collects them as they apply to components.

- **Imported functions.** Calling a function of an imported interface `ns:pkg/iface` (including resource constructors, methods and statics) has the interface effect `ns:pkg/iface`, named in source through the import binding ([08 §8.5](08-effects-and-capabilities.md)). If the WIT result contains a `string` or a `list`, or a parameter contains a list of elements that are not flat-primitive, the call also has `alloc` ([08 §8.14.3](08-effects-and-capabilities.md)). Calling a world-imported function has the world-import effect ([08 §8.5.1](08-effects-and-capabilities.md)).
- **Imported resource drops** have no effect.
- **Exposed functions** (interface impl functions and resource members of exported interfaces, and implementations of world `export FnHead`) MUST have an `effects` clause when their inferred set is non-empty (`MISSING_EFFECTS_DECLARATION`, [08 §8.9.3](08-effects-and-capabilities.md)).
- **Export glue** that receives `string` or `list` parameters, or that returns an owned handle of a resource implemented by the component (every exported constructor does), has the effect `alloc` and is a root of reachability; the complete conditions, for imports and exports, are in [08 §8.14.3](08-effects-and-capabilities.md). A component that exports a resource therefore needs the `alloc` capability.
- **World imports grant nothing.** Each interface that the component imports (§13.5.2) is a required capability, checked against the root's effective policy and against the policy of the package whose code reaches it ([08 §8.12.6](08-effects-and-capabilities.md)). An interface whose types appear in exported signatures is required even if none of its functions is called.
- **Targets.** On a core target every WIT import is `TARGET_CAPABILITY_UNAVAILABLE` (§13.6.2). An interface that the build provides (§13.5.2, N-D-104) is not an import and is available on every target; it is still a required capability.

Example (rejected). `sai.package` of `acme:imaging` has `allow { alloc }` and `export world image_service`, and the impl of `processor` reaches `outgoing_handler.handle`:

```
error[CAPABILITY_NOT_ALLOWED]: capability `wasi:http/outgoing-handler` is required but not allowed by package `acme:imaging`
  = why: export processor.resize   src/lib.sai:31:5
         -> fetch_remote            src/lib.sai:58:12
         -> outgoing_handler.handle src/lib.sai:70:16   imported from wasi:http/outgoing-handler
  = help: add `wasi:http/outgoing-handler` to `allow` in sai.package
```

---

## 13.13 Failures at the boundary

- **Lifting failures trap.** When a value crossing the boundary is not a valid value of its WIT type (a `char` that is not a scalar value, a discriminant out of range, a `string` that is not valid UTF-8, an invalid handle, a pointer or length outside linear memory, misalignment), the Canonical ABI traps ([impl/07](../impl/07-component-lowering.md)). SAI never observes an invalid value.
- **Contracts at exports.** Values received by an exposed function are validated against the invariants of their types and the function's `requires` clauses at run time, in every profile (D-CONTRACT-01, [09-contracts-and-refinements.md](09-contracts-and-refinements.md)). A violation panics, which traps.
- **Panics trap.** A panic inside an export lowers to a trap; it is never converted into a WIT `result` and does not unwind (D-ERR-01, [10-errors-and-panics.md](10-errors-and-panics.md)). Code that wants to report errors to a client returns a `Result` whose error type is declared in the interface.
- **Traps stop the instance.** After a trap the instance cannot be used again; the host decides what happens to the rest of the component graph. A SAI component cannot catch a trap in an import.
- **Async exports** require the Component Model async ABI and are Stage 5 (D-ASYNC-01, R-04). Until then, an `async fn` in an interface is `FEATURE_NOT_IMPLEMENTED`. From Stage 5 on, an exported interface with an `async fn` on a target without the async ABI, which is every v0.1 target, is `ASYNC_EXPORT_UNSUPPORTED` ([11 §11.10.4](11-async-and-concurrency.md), [targets](../targets/targets-and-profiles.md) §3.4 item 1); imported async items are treated in §13.9.2.

---

## 13.14 Decisions introduced by this chapter

Each entry is provisional until it is merged into [decisions.md](../design/decisions.md).

**N-D-28 Resource members are inherent functions of the bound type.** The constructor, methods and statics of an abstract interface resource are implemented by inherent associated functions of the concrete resource type bound with `type R = C`, with the same names (§13.3.3). *Rationale:* D-WIT-01's example binds `Image` but implements only `resize` in the interface impl, so resource members must live with the concrete type (P9); one concrete type is one WIT resource (D-OWN-06).

**N-D-29 Resolution of interface calls.** `iface.f(…)` calls the package's own impl if the package has `impl iface`, and is an import call otherwise. Interfaces of other packages are always imports (§13.3.4): a call of one always crosses a Canonical ABI boundary, whether the build provides the interface within the module, by composition, or not at all (§13.5.2, N-D-104). *Rationale:* the component boundary is explicit and independent of whether the other side is SAI source (P1, P4); library reuse goes through `pub` functions. *Revised:* this entry formerly implied that every such call is an import of the component; since N-D-104, an interface provided within the build is not one, while the call keeps import semantics.

**N-D-30 No interface both imported and exported.** An elaborated world cannot import and export the same interface, or contain duplicate names (`WORLD_DUPLICATE_ITEM`) (§13.4.2). *Rationale:* with N-D-29, a call could not tell the import from the export (P8); WIT middleware worlds that need both are rare and can be added later.

**N-D-31 Commands and reactors.** A component is a command exactly when the root module defines `main`; its world is the export world extended by `include wasi:cli/command`, and `run` returns `ok` exactly for a success `ExitCode` (§13.5.3, §13.10.4). *Rationale:* D-CAP-02 fixes the entry point; WASI 0.2 `run` returns `result` without a code (P4, P9).

**N-D-32 Worlds on core targets.** `sai build` for a core target does not apply the `export world` (information `EXPORT_WORLD_NOT_APPLIED`); `sai component build` for a core target produces a reactor with the world's exports and no imports; `main` there is `COMPONENT_TARGET_MISMATCH` (§13.6.2). *Rationale:* D-TGT-01 (core targets are freestanding; component wrapping has WIT exports only, no WASI); hybrid packages must build on core targets without errors (D-PKG-05, P8); ignoring silently would violate P1, so an information diagnostic is emitted.

**N-D-33 Versionless WIT references in source.** `use` and world items name WIT packages without version; resolution fixes one version per WIT package and resolved node, so two compatibility lines of one package give two versions, and the lock records them (§13.7.1, §13.7.2). *Rationale:* the grammar has no version syntax; source must not change when compatible versions move (P6).

**N-D-34 Unstable WIT items are invisible.** Items with WIT `@unstable` are not part of the SAI view in v0.1 (§13.7.3). *Rationale:* v0.1 has no feature-gate mechanism for WIT, and depending on unstable APIs must be explicit (P1, P8).

**N-D-35 Structural derives for the SAI view.** View enums and flags implement `Copy Clone PartialEq Eq Hash`; view records and variants implement `Clone PartialEq` / `Copy Eq` when all components do; resources none (§13.7.3). *Rationale:* foreign view types cannot be given impls by users (coherence), and the set is computed deterministically from the WIT (P8).

**N-D-36 Name conversion algorithm.** SAI names are derived from WIT names, and WIT names from SAI names, by the algorithms of §13.8.3 and §13.8.4, including the representability conditions and the `r#` / `%` escapes (§13.8.3, §13.8.4). *Rationale:* D-WIT-01 requires a bijection; D-LEX-02 makes the case of each item kind fixed, so the conversion is deterministic (P4, P8).

**N-D-37 Borrowed parameter views.** In the SAI view, `string` and `list<T>` (without handles) in top-level parameters are `Str` and `Slice<T>`; elsewhere `String`, `Vec<T>`, `Bytes`. `Str` and `Slice<T>` are admitted in top-level parameters of local interfaces (§13.9.2). *Rationale:* calling an import must not force a copy (P7); borrowed views cannot escape the call, so D-WIT-01's ban on borrows in results is preserved.

**N-D-38 Flags types.** `@flags` on an all-unit enum in an interface declares a set-valued `Copy` type with constants per flag, `| & ^`, `empty`, `all`, `contains`, `is_empty`, at most 32 flags, no variant patterns; `@flags` elsewhere is rejected in v0.1 (§13.9.4). *Rationale:* WIT flags are sets, not alternatives (P4); the grammar has no flags syntax and D-SYN-09 provides `@flags`; the 32-flag limit is the Component Model's.

**N-D-39 Exact generated WIT format.** Generated WIT is defined byte for byte: its content by §13.11.1, §13.11.3 and §13.11.4, and its text by the format rules of §13.11.2 (§13.11.2). *Rationale:* generated artifacts must be byte-for-byte reproducible for locks, review and semantic diff (determinism principle, P8).

**N-D-40 Contracts are not part of interfaces.** Interface functions cannot carry `borrows`, `requires`, `ensures`, `unsafe` or generics (`INTERFACE_FN_CLAUSE`, `GENERIC_INTERFACE_ITEM`); impls may add contracts (§13.2.2). *Rationale:* WIT cannot express them (P4); D-CONTRACT-01 checks the impl's contracts at the boundary.

**N-D-41 Imports are pruned to what is required.** A component imports only the interfaces required by reachable code and by exported types, and every one of them must be imported by the export world (`WORLD_IMPORT_UNDECLARED`) (§13.5.2). *Rationale:* draft §39 (import only what is needed, P5, P7) together with the world as the explicit contract (P1).

**N-D-42 Methods borrow.** Interface resource methods take `borrow Self` or `borrow mut Self`; a consuming `self: Self` is rejected (§13.2.4). *Rationale:* WIT methods always receive `borrow<r>` (P4); consuming operations are static functions taking `R`.

**N-D-43 External export worlds.** `export world` may name an external world such as `wasi:http/proxy` (§13.5.1). *Rationale:* implementing a standard world must not require copying it (P4, P6).

**N-D-44 Single-type variant payloads.** Interface variants have unit cases or cases with exactly one payload type (§13.2.2). *Rationale:* WIT cases carry at most one type; requiring `A((T, U))` keeps the mapping a bijection (P8, P9).

**N-D-45 Interface record fields are public.** Every field of an interface record is public; a `pub` marker on such a field has no effect (§13.2.2). *Rationale:* WIT records have no private fields (P4).

**N-D-46 Core-level items in component builds.** In a component build, `export fn` items are neither exported nor roots (information `EXPORT_FN_NOT_APPLIED`), and `host` is unavailable, so reachable `extern "wasm"` calls are `TARGET_CAPABILITY_UNAVAILABLE` (§13.6.4). *Rationale:* a component's contract is its world (P1, P4); the Component Model has no raw core imports or exports, and hybrid packages must build on both kinds of target without edits (P6).

**N-D-101 The imports of a composed component.** When artifact dependencies are composed with the root's component, the imports that `W'` bounds are those of the composed component: an interface satisfied inside the composition need not be in `W'`, and an artifact import that remains unsatisfied must be in `W'`, otherwise `WORLD_IMPORT_UNDECLARED` (§13.5.2). *Rationale:* the export world is the contract of the component that the build delivers (§13.5.1), and the host sees only the imports of the composition (P1); the capabilities of satisfied imports are still checked, so composition never hides authority (P5). *Revised:* the rule now covers interfaces provided within the build as well (N-D-104, N-D-106); the text above, which applied only to composition, is unchanged for it.

**N-D-104 In-build provision by hybrid dependencies in source form.** A hybrid dependency used in source form is compiled with its `export world` applied and provides, within the build, every interface that world exports: each call of a function of such an interface, from any package of the build, is linked to the provider's impl through the Canonical ABI glue of both sides inside one core module, on every target; values are copied, view types and the provider's types stay distinct, trait impls do not cross, and the call keeps its interface effect. The provider of `ns:pkg/iface@v` is the active node of identity `ns:pkg` and version `v`, never for the `wasi` namespace, never for world functions (§13.5.1, §13.5.2, §13.6.2). *Rationale:* D-PKG-05 requires that a hybrid works in both forms, and P8 requires that the form does not change behaviour; fusing the boundary inside the module gives source form the semantics of artifact form, also for `tiny` and core targets, where composition is not available. *Revised:* this replaces the former rule that a hybrid in source form is never composed and provides nothing, which made source form a dead end on core targets.

**N-D-105 Provided interfaces stay inside the component.** A provided interface is not exported by `W'`, its types occur in no boundary type of an export of `W'` or of an interface that remains an import, and no composed artifact imports an interface provided within the build; otherwise `PROVIDED_INTERFACE_AT_BOUNDARY` (§13.5.2). *Rationale:* the Component Model requires the named types of a component's imports and exports to be imported or exported themselves, and handles of a provider inside the module are not component handles, so crossing the boundary would mix two handle spaces or two instances of one provider (P4, P8); rejecting it keeps the fused call equal to the composed one.

**N-D-106 Provided interfaces are capabilities, not imports.** An interface in `Imp` that the build provides is not an import of the component and need not be imported by `W'`, but it remains a required capability; the interfaces that an in-build provider calls from reachable code are in `Imp` like those of any reachable body (§13.5.2, §13.6.2). *Rationale:* the export world bounds what the host must supply (P1), and the policies bound authority whatever the form (P5, P8).

---

## 13.15 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `GENERIC_INTERFACE_ITEM` | error | generic parameters or a `where` clause on an interface item or interface impl function (§13.2.2, §13.3.1) |
| `INTERFACE_FN_CLAUSE` | error | `borrows`, `requires`, `ensures` or `unsafe` on an interface function or resource member (§13.2.2) |
| `INTERFACE_FN_BODY` | error | an interface function, interface resource member or world function has a body; reported by the parser instead of `UNEXPECTED_FN_BODY`, which is only for `extern` blocks (§13.2.2, §13.4.1) |
| `WIT_EXPLICIT_DISCRIMINANT` | error | an explicit discriminant on a variant of an interface enum (§13.2.2) |
| `WIT_EMPTY_TYPE` | error | an interface record without fields, or an enum or flags without variants (§13.2.2) |
| `WIT_INCOMPATIBLE_TYPE` | error | a boundary type, payload shape, receiver or `@repr` that has no WIT mapping (§13.2.2, §13.2.4, §13.9.3) |
| `INVALID_FLAGS_ENUM` | error | `@flags` on an enum with payloads, discriminants, more than 32 variants, or outside an interface (§13.9.4) |
| `FLAGS_PATTERN` | error | a variant pattern on a flags type (§13.9.4) |
| `INTERFACE_USE_CYCLE` | error | local interfaces use each other's types cyclically (§13.2.3) |
| `IMPL_TARGET_NOT_INTERFACE` | error | the path of an interface impl does not resolve to an interface (§13.3.1) |
| `DUPLICATE_INTERFACE_IMPL` | error | a second `impl` of the same interface in one package (§13.3.1) |
| `INTERFACE_IMPL_MISSING_ITEM` | error | an interface impl or bound resource lacks a function, member or resource binding (§13.3.1, §13.3.3) |
| `INTERFACE_IMPL_EXTRA_ITEM` | error | an interface impl contains an item the interface does not declare (§13.3.1) |
| `INTERFACE_IMPL_SIGNATURE_MISMATCH` | error | an implementing function does not conform to the declared signature (§13.3.2, §13.4.1) |
| `INVALID_RESOURCE_BINDING` | error | `type R = C` where `C` is not a non-generic local resource, or `C` is bound twice (§13.3.3) |
| `INVALID_WORLD_ITEM` | error | a world item names a world where an interface is required, or vice versa (§13.4.1) |
| `WORLD_EXPORT_UNIMPLEMENTED` | error | an exported interface or world function has no implementation (§13.4.1) |
| `WORLD_INCLUDE_CYCLE` | error | worlds include each other cyclically (§13.4.2) |
| `WORLD_DUPLICATE_ITEM` | error | an elaborated world imports and exports the same interface, or repeats a name (§13.4.2, §13.5.3) |
| `UNKNOWN_EXPORT_WORLD` | error | `export world` names no world (§13.5.1) |
| `WORLD_IMPORT_UNDECLARED` | error | the component requires an interface that the export world does not import (§13.5.2) |
| `PROVIDED_INTERFACE_AT_BOUNDARY` | error | a provided interface is exported by the export world, has a type in a boundary type of an export or of a remaining import, or, when provided within the build, is imported by a composed artifact (§13.5.2) |
| `NO_COMPONENT_EXPORTS` | error | a component build of a reactor with nothing to export (§13.5.3) |
| `EXPORT_WORLD_NOT_APPLIED` | info | `sai build` for a core or native target ignores the `export world` (§13.6.1, §13.6.2) |
| `EXPORT_FN_NOT_APPLIED` | info | a component build does not export the root package's `export fn` items (§13.6.4) |
| `COMPONENT_TARGET_MISMATCH` | error | a component build for a core target of a package that defines `main` (§13.6.2), or `sai component build` for a native target (§13.6.1) |
| `WIT_PARSE_ERROR` | error | a visible WIT package fails to parse or validate (§13.7.1) |
| `UNKNOWN_WIT_ITEM` | error | a `use` path names no interface or world of a WIT package (§13.7.2) |
| `OPAQUE_RESOURCE_CONSTRUCTION` | error | record-construction syntax used with an imported resource type (§13.7.4) |
| `WIT_NAME_NOT_REPRESENTABLE` | error | a SAI identifier that must cross the boundary has no WIT name (§13.8.4) |
| `WIT_NAME_COLLISION` | error | two SAI items map to the same WIT name in one scope (§13.8.5) |
| `WIT_ITEM_SHADOWED` | warning | an item of imported WIT is not accessible from SAI because its SAI name collides with another item, such as a static function `new` next to a constructor (§13.7.4, §13.8.5) |
| `WIT_UNSUPPORTED_FEATURE` | error | use of a WIT item whose signature contains a construct not supported in v0.1, or, from Stage 5 on, an imported `async` function or a `future` or `stream` type on a target without the Component Model async ABI (§13.9.2) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | defined in [03](03-grammar.md) §3.9; see §13.4.1 |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | defined in [03](03-grammar.md) §3.7; see §13.3.1 |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | defined in [03](03-grammar.md) §3.8; see §13.9.3 |
| `NON_ASCII_PUBLIC_NAME` | error | defined in [02](02-lexical-structure.md) §2.4; see §13.2.1 |
| `EFFECTS_ON_INTERFACE_FN` | error | defined in [08](08-effects-and-capabilities.md) §8.6.1; see §13.2.2 |
| `MISSING_EFFECTS_DECLARATION` | error | defined in [08-effects-and-capabilities.md](08-effects-and-capabilities.md) §8.9.3 (§13.3.2, §13.12) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; see §13.6.2 |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; see §13.12 |
| `AMBIENT_IMPORT` | warning (lint) | defined in [08-effects-and-capabilities.md](08-effects-and-capabilities.md) §8.5.3 (§13.6.3) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01-introduction.md](01-introduction.md) §1.4.5 (§13.2.2, §13.9.2, §13.13) |
| `ASYNC_EXPORT_UNSUPPORTED` | error | defined in [11-async-and-concurrency.md](11-async-and-concurrency.md) §11.10.4 (§13.13) |
