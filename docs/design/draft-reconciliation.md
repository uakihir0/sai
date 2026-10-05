# Draft Reconciliation

**Status:** Informative (each entry points to the normative rule that decides it)
**Scope:** Every place where this specification departs from, corrects, or makes precise the language draft `specification.txt` (v0.1 Draft).

The draft states the philosophy and the shape of SAI. Several of its examples do not type-check under the precise rules this specification had to adopt, and some sections leave choices open. This document lists every such case, so that a reader of the draft can see what changed and why.

Entries are grouped as:

- **Corrections** — a draft example is not valid SAI under the final rules; the corrected form is given.
- **Changes** — the draft's design was revised.
- **Precisions** — the draft left something open; the decision is summarized here.
- **Additions** — constructs the draft does not mention, introduced to make the language complete.
- **Open issues** — known gaps deliberately left for after v0.1.

---

## 1. Corrections to draft examples

### R-01 Unqualified `HttpClient` (draft §25, §80)

The draft writes `use sai:http/client` and then uses `HttpClient` unqualified. `use` with a module path binds only the module (`client`); it never imports items implicitly, and there are no glob imports (D-MOD-01, P1).

```sai
use sai:http/client.{HttpClient} // corrected: import the item explicitly
// or: use sai:http/client  and write client.HttpClient
```

### R-02 String concatenation with `String` on the right (draft §80)

`"/users/" + id.to_string()` is `Str + String`, which does not exist. Only `Str + Str -> String` and `String + Str -> String` are provided, because there is no implicit `String` to `Str` coercion (D-TYPE-04, D-EXPR-13).

```sai
http.get("/users/" + id.value.to_string().as_str()).await
```

`UserId` is `distinct UserId = U64`. A distinct type inherits no methods of its base (D-TYPE-03), so the conversion goes through `id.value` (accessible inside the defining package).

### R-03 `?` across different error types (draft §80)

`http.get(..).await?` yields an `HttpError`, and `response.json<User>()?` yields a decoding error, while `fetch_user` returns `Result<User, UserError>`. `?` requires identical error types and has no implicit `From` conversion (D-EXPR-05, P1).

```sai
let response = http.get(path.as_str()).await.map_err(fn(_) { UserError.Network })?
let user = response.json<User>().map_err(fn(_) { UserError.InvalidResponse })?
```

### R-04 Corrected complete example (draft §80)

```sai
use sai:http/client.{HttpClient}

pub distinct UserId = U64

pub type User {
    id: UserId
    name: String
}

pub enum UserError {
    NotFound
    Network
    InvalidResponse
}

pub async fn fetch_user(http: borrow HttpClient, id: UserId) -> Result<User, UserError>
    effects { alloc, net.http }
{
    let path = "/users/" + id.value.to_string().as_str()
    let response = http.get(path.as_str()).await.map_err(fn(_) { UserError.Network })?

    if response.status == 404 {
        return Err(.NotFound)
    }

    let user = response.json<User>().map_err(fn(_) { UserError.InvalidResponse })?

    Ok(user)
}
```

The package contract of §80 says `export world client`, but the example defines no world named `client`. The package must declare one (D-WIT-01). `fetch_user` itself cannot be exported through it as written: `HttpClient` is a library capability value, not a WIT type, and `UserId` and `User` are declared outside any interface (R-08). An exported entry point declares its own WIT-compatible types and obtains the HTTP client from the component's imports:

```sai
interface users {
    enum UserError {
        NotFound
        Network
        InvalidResponse
    }

    type User {
        id: U64
        name: String
    }

    fn fetch_user(id: U64) -> Result<User, UserError>
}

world client {
    import wasi:http/outgoing-handler
    export users
}
```

Exporting an `async` function requires the Component Model async ABI (Stage 5, D-ASYNC-01). Until then, `impl users` implements `fetch_user` synchronously, for example by driving the future with the standard executor.

`response.json<User>()` also requires `User` to implement the decoding trait of the JSON library. See O-01.

### R-05 Literal compared with a distinct type (draft §23, §24)

`ensures result >= 0` and `self.balance >= 0`, where the value has type `Money`. A literal never has a distinct type (D-TYPE-03), so the constant must be constructed.

```sai
fn withdraw(balance: Money, amount: Money) -> Money
    requires amount <= balance
    ensures result >= Money(0)
{
    balance - amount
}

invariant Account { self.balance >= Money(0) }
```

`Money` needs `@derive(Copy, Clone, PartialEq, PartialOrd, Sub)`, for example `distinct Money = I64`. `Copy` requires `Clone`, and `PartialOrd` requires `PartialEq`; supertraits are never derived implicitly ([spec/05](../spec/05-types.md) §5.12.1). With derived operators the Standard Fact Checker sees through the distinct type and proves the `ensures` clause statically (D-CONTRACT-02).

### R-06 `self.bytes` in a refinement (draft §13)

`type NonEmptyStr = Str where self.bytes > 0`: `Str` has no `bytes` field. The predicate uses the length method.

```sai
type NonEmptyStr = Str where self.len() > 0
```

`self.len()` is a pure method call. The SFC treats it as an opaque atom, which is still enough to prove the predicate after a dominating `if s.len() > 0` check (D-CONTRACT-02, D-CONTRACT-03).

### R-07 Property test that reuses a moved value (draft §54)

`reverse(reverse(xs)) == xs` moves `xs` into `reverse` and then uses it again, which is a use after move (D-OWN-01).

```sai
property "reverse twice" (xs: Vec<I32>) {
    reverse(reverse(xs.clone())) == xs
}
```

### R-08 Types used by an interface must be declared in it (draft §42)

The `processor` interface uses `ImageError` without declaring it. A nominal type defined outside an interface cannot appear in the interface's signatures, because every such type must have a WIT definition (D-WIT-01). `Bytes` is allowed: it is the alias `Vec<Byte>` and maps to `list<u8>`.

```sai
interface processor {
    enum ImageError {
        TooLarge
        Unsupported
    }

    fn resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, ImageError>
}
```

### R-09 Unsafe module syntax (draft §69)

`unsafe module decoder { ... }` would be an inline module, and SAI has none: a file is a module (D-SYN-08). The unsafe marker becomes a header at the top of the file `decoder.sai`.

```sai
//! Low-level decoder.
unsafe module

fn decode(...) { ... }
```

### R-10 Root module file name (draft §27, §34)

The draft's source package shows `src/json.sai` as the only file. The root module of a package is always `<source>/lib.sai` (D-SYN-08). With the draft's layout, `src/json.sai` would be the module `json`, reached as `use acme:json/json`. The corrected layout is:

```text
acme-json/
├ sai.package
└ src/
   └ lib.sai
```

### R-11 Typo in the lock file name (draft §33)

The text "`sa​​i.lock`" contains two zero-width spaces (U+200B). The file name is `sai.lock`.

### R-12 FFI types are library types (draft §38)

`CStr`, `CInt`, `Ptr<T>`, and `Out<T>` in the `extern "C"` example are not built-in names, and must be imported:

- `Ptr<T>` and `PtrMut<T>` are the raw pointer types of `sai:core/ptr` (Stage 2). `sai:core/ffi` re-exports them.
- `CStr`, the C integer aliases such as `CInt`, and `Out<T>`, the out-parameter pointer type, live in `sai:core/ffi` (Stage 7). The owned `CString` lives in `sai:alloc/ffi` (Stage 7).
- The draft's block also has to be `unsafe extern "C"` with a library name. See [spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.9 and [spec/16-core-library.md](../spec/16-core-library.md) §16.12.

```sai
use sai:core/ffi.{CInt, CStr, Out, Ptr}

unsafe extern "C" module "sqlite3" {
    fn sqlite3_open(filename: CStr, db: Out<Ptr<Sqlite>>) -> CInt
}
```

### R-13 Capability names of dependencies (draft §74)

`wasi:filesystem/read` and `wasi:filesystem/write` are illustrative and are not real WASI interface names. Capability names are WIT package names or actual interface names (D-CAP-01), for example `wasi:filesystem` or `wasi:filesystem/types`. Read and write are distinguished at the effect level (`fs.read`, `fs.write`), not at the capability level.

### R-14 Explicit borrows at call sites (draft §16.2, §21)

The draft shows `fn width(image: borrow Image)` but no call. Arguments are never borrowed implicitly: the call is written `width(borrow image)` (D-OWN-02, P1). Only method receivers are auto-referenced.

### R-15 Artifact keys are target names (draft §35)

The draft's hybrid package writes the artifact key `wasm32`, which is not a target name. Artifact keys and `targets` entries use the full target names of D-TGT-01, and a hybrid package also declares `wit` and `export world`, the contract that its source and its artifact both implement ([package/01](../package/01-package-contract.md) §1.18.4).

```sai-package
artifact component {
    wasm32-wasi "dist/image.wasm"
}
targets { wasm32-core, wasm32-wasi }
```

### R-16 `native_ffi` is a capability, not a policy item (draft §68)

The draft lists `deny native_ffi` next to `deny unsafe` inside `policy`. `native_ffi` is a capability (D-CAP-01), so it is denied with `deny { native_ffi }`. Writing it inside `policy` is `POLICY_ITEM_IS_CAPABILITY` ([package/01](../package/01-package-contract.md) §1.12, §1.18.6).

```sai-package
policy {
    deny unsafe
}
deny { native_ffi }
```

---

## 2. Changes to the draft's design

### C-01 World imports name interfaces, not packages (draft §43)

The draft writes `import wasi:http` in a world. World imports and exports are per interface (D-WIT-03).

```sai
world image_service {
    import wasi:http/outgoing-handler
    export processor
}
```

Package-level grants remain available where they belong, in the capability policy: `allow { wasi:http }`.

### C-02 Futures without `Pin` (draft §25)

The draft only names `Future<T>` and `Stream<T>`. The final model (D-ASYNC-01):

- `Future` is a trait with an associated type `Output`. The draft's `Future<T>` is read as "a type implementing `Future<Output = T>`". Because there are no trait objects or opaque return types in v0.1, `Future<T>` is not itself a type.
- `poll` is an `unsafe fn` whose contract forbids moving a future after its first poll. Safe code drives futures only through `.await` and `for await`.
- As a result, borrows of frame-local values may be held across `.await`, and there is no `Pin` type.

### C-03 Profiles never change semantics (draft §46, §55)

The draft lists "panic = trap" as a tiny-profile setting. In every profile a panic ends in a trap. The profiles differ only in whether the message is reported first (D-ERR-01, D-TGT-02). Overflow, bounds, and contract checks are present in every profile (D-TYPE-06, D-CONTRACT-01).

### C-04 Effect declarations: when they are required (draft §20.1)

The draft says effect declarations are "recommended" for public APIs and "required" for stable APIs. Precisely (D-EFF-01):

- An omitted effects clause always means "infer".
- A non-empty inferred effect set must be written explicitly for `export` functions, for functions exposed through worlds and interfaces, and for `pub` functions of packages at version 1.0.0 or later.
- Below 1.0.0, a `pub` function with inferred non-empty effects gets the warning `PUBLIC_EFFECTS_INFERRED`.

### C-05 Borrow regions are not inferred from function bodies (draft §16.3)

The draft says the compiler infers borrow regions. Inside function bodies it does (NLL). Across function boundaries, the borrowing contract of a function is determined by its signature alone, through fixed elision rules plus an optional `borrows { .. }` clause (D-OWN-03). This keeps checking modular and makes semantic diffs of signatures meaningful (P2, P8).

### C-06 `--tiny` (draft §35)

The draft mentions `--tiny`. The CLI spelling is `--profile tiny`, the only way to select a profile (P9).

### C-07 Target capability names (draft §59)

The draft's examples (`memory.linear`, `virtual memory`, `gpu buffers`, ...) describe target capabilities informally. The capability names that can appear in a policy are fixed by D-CAP-01 (`threads`, `host`, `native_ffi`, `process`, `memory64`, plus `alloc` and WIT names). The target table is in [targets/targets-and-profiles.md](../targets/targets-and-profiles.md).

### C-08 Symbol ID format (draft §64)

The draft's example `symbol://acme:user/User/create#8F21A7` matches the final format `symbol://<ns>:<pkg>/<module path>/<item>[/<member>]#<6 hex digits>`, with an empty module path for the root module (D-TOOL-02).

### C-09 The tiny profile does not strip exports (draft §46)

The draft lists "strip unused exports" among the settings of the tiny profile. A compiler cannot know which exports a host uses, and an export that disappears in one profile makes a module that instantiates and runs in `dev` fail in `tiny`. The imports and exports of a module are therefore fixed by reachability before optimization and are the same in every profile ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.5, N-F-20; C-03). What the draft's list asks for is done by dead-code elimination, in every profile: code that no export or entry point reaches, including the allocator and Unicode tables when nothing uses them, is not in the module ([impl/09](../impl/09-optimization-and-size.md) §9.6, N-F-52). The profiles differ in optimization and in metadata, such as the name section and debug information (D-TGT-02, [targets](../targets/targets-and-profiles.md) §5.2).

---

## 3. Precisions

The draft leaves these points open. Each is decided in the decision record:

| Topic | Draft | Decision |
|---|---|---|
| Statement termination | not specified | newline rules, no semicolons (D-LEX-01) |
| Identifier case | conventions only | first-letter case is significant (D-LEX-02) |
| Struct literals | not shown | `User(id: .., name: ..)` with parentheses (D-SYN-01) |
| Enum variant reference | `Err(.NotFound)` | `Type.Variant` or implicit member `.Variant` (D-SYN-02) |
| Path separator | `.` in examples | `.` only, no `::` (D-SYN-03) |
| Generic call syntax | `json<User>()` | no turbofish; disambiguation rule (D-SYN-04) |
| Logical operators | `and` in §13 | `and` / `or` / `not`; no `!` (D-SYN-05) |
| Signature clause order | examples only | fixed order (D-SYN-06) |
| Visibility of `export` | §81 | `export` implies `pub`; functions only (D-MOD-02) |
| Integer overflow | not specified | panics in every profile; wrapping operators `+%` etc. (D-TYPE-06) |
| Refinement conversion | "validation at safe boundaries" | statically proven or explicit `try_from`; compile error otherwise (D-TYPE-03, D-CONTRACT-01) |
| Contract checks | "runtime validation as needed" | runtime checks kept in every profile when unproven (D-CONTRACT-01) |
| Which proofs count | "if the compiler can prove" | the Standard Fact Checker defines acceptance (D-CONTRACT-02) |
| Capability default | not specified | an omitted `allow` means the empty set (D-CAP-01) |
| Version resolution | not specified | Minimal Version Selection, no backtracking (D-PKG-02) |
| Lock file format | conceptual list | declarative line format, `sai1:` checksums (D-PKG-03) |
| Hybrid package choice | "the best form" | deterministic rules, override in root (D-PKG-05) |
| `wasm32-wasi` output | not specified | a WASI 0.2+ component (D-TGT-01) |
| Structured concurrency | "assumes structured concurrency" (§25) | child tasks exist only inside `scope(fn(s) { .. })` of `sai:async` and are spawned with `s.spawn(..)`; there are no detached tasks (D-ASYNC-01) |
| Scheduler | "not forced" (§25.3) | the executor of `sai:async` is an ordinary library, linked only when used; `block_on` is deterministic ([spec/11](../spec/11-async-and-concurrency.md)) |
| Tests and properties | §54 | tests are hermetic (capabilities within `{ alloc }`), property inputs come from a deterministic seed, and there is no shrinking in v0.1 ([spec/14](../spec/14-testing.md)) |
| Diagnostic codes | §60 | one owner per code, one severity, and a fixed list of lints ([spec/15](../spec/15-attributes-and-lints.md) §15.17, [appendix A](../spec/appendix-a-diagnostic-codes.md)) |

---

## 4. Additions

Constructs that the draft does not mention, and why they were needed:

| Addition | Reason | Decision |
|---|---|---|
| `borrows { .. }` clause | express which inputs a returned borrow comes from without lifetime syntax | D-OWN-03 |
| `effect name covers { .. }` | user-defined, opaque effect names for libraries | D-EFF-03 |
| Effects on bounds and function pointer types | effect-polymorphic higher-order functions without effect variables | D-EFF-02 |
| `owned T` as a synonym | the draft writes `owned File` | D-OWN-01 |
| `@attributes` (closed set) | derive, inlining, lint levels, export names, without macros | D-SYN-09 |
| `pub use` re-exports | stable public APIs over internal module structure | D-MOD-04 |
| Arena API in `region` blocks | make the draft's `region` concrete, with explicit allocation | D-OWN-07 |
| `extern "wasm" module ".."` | raw core Wasm imports, gated by the `host` capability | D-UNSAFE-01 |
| `impl <interface>` | implementing an exported WIT interface | D-WIT-01 |
| `Byte` vs `U8` rules | the draft says `Byte` is binary compatible with `U8` but semantically different | D-TYPE-01 |
| Wrapping and checked arithmetic | overflow panics, so explicit alternatives are needed | D-TYPE-06 |
| `Array.filled(x)` | arrays of a repeated value without a dedicated syntax | D-SYN-10 |
| Format strings `f"..."` (Stage 3) | readable message building without macros | D-EXPR-12 |

---

## 5. Open issues (after v0.1)

### O-01 Deriving library traits

`response.json<User>()` (draft §80) needs `User` to implement a decoding trait of the JSON library. v0.1 has no macros and the `@derive` list is closed (D-SYN-09, D-TYPE-08), so such impls are written by hand. A post-v0.1 design should let libraries offer derivations through compile-time metadata (draft §70) without arbitrary token rewriting (draft §83).

### O-02 Trait objects and opaque return types

There is no `dyn Trait` and no `impl Trait` return type in v0.1 (D-TYPE-02, D-TYPE-09). Closures and futures can be returned only through generics or concrete types. Both features are candidates for a later version.

### O-03 Linear types

Resources are affine (D-OWN-06). Types that must be consumed explicitly (for example, a transaction that must be committed or rolled back) are a future extension.

### O-04 Package registries and signing

Git and path sources only (D-PKG-01). Registries and package signing (draft §73) are future work.

---

## 6. Revising the draft

The draft `specification.txt` is kept unchanged as the historical source, outside this repository. The specification in `docs/` replaces it. When the draft is next revised, the corrections in §1 and the changes in §2 should be applied to its examples.
