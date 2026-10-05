# 07. Component Lowering

**Status:** Normative for the reference implementation
**Stage:** 4 (async ABI: Stage 5; `wasm64-wasi`: experimental)
**Related:** [../spec/13-components-and-wit.md](../spec/13-components-and-wit.md), [../spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md), [01-architecture.md](01-architecture.md), [04-mir.md](04-mir.md), [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md), [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), [09-optimization-and-size.md](09-optimization-and-size.md), [10-runtime.md](10-runtime.md), [15-conformance-and-testing.md](15-conformance-and-testing.md), [16-interfaces-and-wit.md](16-interfaces-and-wit.md), [../targets/targets-and-profiles.md](../targets/targets-and-profiles.md), [../package/04-distribution-and-security.md](../package/04-distribution-and-security.md), [decisions.md](../design/decisions.md) D-WASM-01..05, D-TGT-01, D-OWN-06, D-ERR-01, D-CONTRACT-01, D-TOOL-05

This document specifies how the reference implementation turns a SAI program with WIT imports and exports into a WebAssembly component: the Canonical ABI size, alignment and flattening of every mapped WIT type, the lift and lower conversions between SAI's internal representation ([06](06-wasm-lowering-and-layout.md)) and the Canonical ABI, `cabi_realloc`, post-return functions and the ownership of boundary memory, resource tables and handles, the glue generated for every import and export, the traps of the Canonical ABI, the assembly of the core module, its wrapping into a component and the composition with artifact dependencies, commands and reactors, the naming of core imports and exports, and the validation of the output. Source-level semantics are in [spec/13](../spec/13-components-and-wit.md); this document never changes them. The Canonical ABI itself is the one of the Component Model specification for the WASI version that the target fixes; where this document restates it, the Component Model specification is authoritative for the runtime's behaviour, and this document is authoritative for what the SAI compiler generates.

---

## 7.1 Overview

### 7.1.1 Pipeline

Component lowering runs after monomorphization (MIR phase `Mono`, D-MIR-01) and before core-Wasm emission ([06](06-wasm-lowering-and-layout.md)). Its inputs are:

- the elaborated world `W'` and the import set `Imp` of [spec/13 §13.5](../spec/13-components-and-wit.md), computed from the reachability of [impl/08](08-effect-and-capability-analysis.md);
- the item table of [09](09-optimization-and-size.md) §9.6.2, whose instances are those of `mono_instances` ([04](04-mir.md) §4.13.1), and the type layouts of [06](06-wasm-lowering-and-layout.md);
- the resolved WIT packages (`wit-parser` `Resolve`, D-TOOL-05).

The stages are:

1. **WIT resolution.** Build one `Resolve` containing the root's generated WIT package ([spec/13 §13.11](../spec/13-components-and-wit.md)), every WIT package it references at its resolved version, and the synthetic world of §7.11.2. The `Resolve` is built by `sai_wit::resolve_for` ([16](16-interfaces-and-wit.md) §16.10.2) from the WIT model and the elaborated worlds of [16](16-interfaces-and-wit.md) §16.8.3.
2. **Signature computation.** For every function in `Imp` and every export of `W'`, compute the Canonical ABI core signature (§7.3).
3. **Glue generation.** Generate one import glue body per reachable imported function (§7.7), one export glue body per export (§7.8), the post-return functions (§7.5), the resource intrinsics wrappers and destructors (§7.6), and the provision glue of the interfaces provided within the build (§7.11.10). Glue is generated as MIR in phase `Mono`, marked *synthetic*; it is not borrow-checked (it is generated after phase `Analyzed`) and uses the operations of [04](04-mir.md) §4.13.9 "Raw memory in synthetic bodies", of which glue uses only the runtime intrinsics `rt.copy` and `rt.fill` of [10](10-runtime.md) §10.1.2 (N-D-64). `cabi_realloc` is not glue: it is the runtime body of [10](10-runtime.md) §10.3, built by `runtime_body` and rooted by [04](04-mir.md) §4.13.1 when the component exports it.
4. **Core emission.** [06](06-wasm-lowering-and-layout.md) lowers all bodies, including glue, into one core module. Glue participates in optimization like any other body ([09](09-optimization-and-size.md)), except that core exports and core imports are never removed or renamed.
5. **Metadata.** Embed the `component-type` custom section (§7.11.1).
6. **Wrapping.** Encode the component with `wit-component` (§7.11.3). If the build uses artifact dependencies, compose it with their artifacts (§7.11.9).
7. **Validation.** Validate the core module and the component (§7.13).

`glue_bodies` does not depend on `mono_instances`; a glue body calls only the instances that [04](04-mir.md) §4.13.1 roots for glue (the export targets, `BoundaryEntry`, the drop glue and the runtime bodies), provides the drop body of an imported resource (§7.6.2) that [04](04-mir.md) §4.13.5 uses, and sets the `SourceInfo.span` of each of its statements and terminators ([04](04-mir.md) §4.13.7 item 2).

For a core target without `sai component build`, stages 1 to 3 run only for the interfaces provided within the build (§7.11.10), stages 5 and 6 do not run, and the module has no Canonical ABI glue other than that of §7.11.10 ([spec/13 §13.6.2](../spec/13-components-and-wit.md)). Without such an interface, only stages 4 and 7 run.

### 7.1.2 Determinism

Given the same inputs, every stage MUST produce byte-identical output. In particular: core imports are emitted in the order of §7.10.3; core exports in the order of §7.10.4; glue bodies are generated in the same order as their imports or exports; custom sections contain no timestamps, absolute paths or host-dependent data.

### 7.1.3 Notation

- `size(t)`, `align(t)`: Canonical ABI size and alignment of WIT type `t` in linear memory (§7.2).
- `flat(t)`: the list of core value types of `t` after flattening (§7.3.1).
- `layout(T)`: SAI's internal layout of SAI type `T` ([06](06-wasm-lowering-and-layout.md), D-WASM-03).
- `align_to(x, a) = ⌈x / a⌉ · a`.
- `ptr` denotes the core pointer type: `i32` on wasm32 targets and `i64` on `wasm64-wasi` (§7.11.7) and in the provision glue of a wasm64 target (§7.11.10). All tables below are written for wasm32.

---

## 7.2 Canonical ABI types, size and alignment

### 7.2.1 Despecialization

Before size, alignment and flattening are computed, specialized WIT types are rewritten into their fundamental forms:

| WIT type | Fundamental form |
|---|---|
| `tuple<t1, …, tn>` | `record { 0: t1, …, n-1: tn }` |
| `enum { c1, …, cn }` | `variant { c1, …, cn }` (no payloads) |
| `option<t>` | `variant { none, some(t) }` |
| `result<t, e>` | `variant { ok(t), error(e) }`; an omitted `t` or `e` is a case without payload |

`flags` and `string` are fundamental. A type alias is replaced by its target.

### 7.2.2 Size and alignment

| Type | `size` | `align` |
|---|---|---|
| `bool`, `s8`, `u8` | 1 | 1 |
| `s16`, `u16` | 2 | 2 |
| `s32`, `u32`, `f32`, `char` | 4 | 4 |
| `s64`, `u64`, `f64` | 8 | 8 |
| `string`, `list<t>` | 8 (pointer at offset 0, length at offset 4) | 4 |
| `own<r>`, `borrow<r>` | 4 | 4 |
| `flags` with n flags, 1 ≤ n ≤ 8 | 1 | 1 |
| `flags`, 9 ≤ n ≤ 16 | 2 | 2 |
| `flags`, 17 ≤ n ≤ 32 | 4 | 4 |
| `record { f1, …, fk }` | see below | max over `align(fi)` |
| `variant { c1, …, cn }` | see below | max(`align(disc(n))`, max over payload alignments) |

Records:

```
size(record) = align_to(s_k, align(record))   where s_0 = 0,
               s_i = align_to(s_{i-1}, align(f_i)) + size(f_i)
offset(f_i)  = align_to(s_{i-1}, align(f_i))
```

Fields are laid out in WIT declaration order, which is SAI declaration order ([spec/13 §13.7.3](../spec/13-components-and-wit.md)).

Variants:

```
disc(n)        = u8 if n ≤ 2^8, u16 if n ≤ 2^16, u32 otherwise
payload_align  = max(1, max over cases of align(payload))
payload_offset = align_to(size(disc(n)), payload_align)
size(variant)  = align_to(payload_offset + max(0, max over cases of size(payload)), align(variant))
```

The discriminant of case `ci` (zero-based declaration order) is `i`. In SAI's internal layout the tag is also chosen by case count, but niche filling can remove it (D-WASM-03), so the two layouts are never assumed to agree for variants (§7.9).

### 7.2.3 Strings and lists in memory

- `string` uses the `utf8` string encoding (canonical option `string-encoding=utf8`). The pointer points to `len` bytes of UTF-8 data with alignment 1; the length is in **bytes**.
- `list<t>` points to `len` elements of `size(t)` bytes each, stored consecutively at stride `size(t)`, with the pointer aligned to `align(t)`.
- For `len = 0` the pointer is any value aligned to the element alignment; SAI always passes the element alignment itself (a non-null, aligned address inside the null guard of D-WASM-02, never dereferenced).

### 7.2.4 Flags in memory

A `flags` type with flags `f0, …, f(n-1)` (WIT declaration order, which is SAI variant order, [spec/13 §13.9.4](../spec/13-components-and-wit.md)) is an unsigned integer of `size(flags)` bytes in which bit `i` (value `2^i`) is set exactly when `fi` is in the set. Bits `n` and above are zero in every value that a SAI operation produces. A value lifted from the boundary can have them set, and the flags operations ignore them ([16](16-interfaces-and-wit.md) §16.9.6). The internal layout is the same integer ([06](06-wasm-lowering-and-layout.md) §6.4.18). SAI limits flags types to 32 flags (spec/13 N-D-38), so the Canonical ABI's multi-word flags never occur.

### 7.2.5 Handles in memory

`own<r>` and `borrow<r>` are stored as a 32-bit handle index. Index 0 is never a valid handle. In linear memory of a SAI component, handles occur only inside parameter areas, return areas and list buffers that cross the boundary; SAI values hold imported handles directly (§7.6.2).

---

## 7.3 Flattening and core signatures

### 7.3.1 Flattening

`flat(t)` is the sequence of core value types that represents `t` when it is passed as a core parameter or result:

| Type | `flat(t)` |
|---|---|
| `bool`, `s8`, `u8`, `s16`, `u16`, `s32`, `u32`, `char` | `[i32]` |
| `s64`, `u64` | `[i64]` |
| `f32` | `[f32]` |
| `f64` | `[f64]` |
| `string`, `list<t>` | `[i32, i32]` (pointer, length) |
| `own<r>`, `borrow<r>` | `[i32]` |
| `flags` (1 to 32 flags) | `[i32]` |
| `record { f1, …, fk }` | `flat(f1) ++ … ++ flat(fk)` |
| `variant { c1, …, cn }` | `[i32] ++ joined(c1, …, cn)` |

`joined` merges the flattenings of the case payloads position by position:

```
joined(cases):
    out := []
    for c in cases, in declaration order, that have a payload p:
        for (i, ty) in enumerate(flat(p)):
            if i < len(out): out[i] := join(out[i], ty)
            else:            out.push(ty)
    return out

join(a, b):
    if a == b:                return a
    if {a, b} == {i32, f32}:  return i32
    otherwise:                return i64
```

The leading `i32` is the discriminant, whatever `size(disc(n))` is.

### 7.3.2 Coercions into and out of joined positions

When the payload of the active case is placed into the joined positions, each payload value of type `ty` at position `i` is coerced to `out[i]`; positions beyond the payload, and all positions of a case without payload, are filled with zero of their type (`i32.const 0`, `i64.const 0`, `f32.const 0`, `f64.const 0`). Reading reverses the coercion:

| Payload type → joined type | Lowering (write) | Lifting (read) |
|---|---|---|
| same | none | none |
| `f32` → `i32` | `i32.reinterpret_f32` | `f32.reinterpret_i32` |
| `i32` → `i64` | `i64.extend_i32_u` | `i32.wrap_i64` |
| `f32` → `i64` | `i32.reinterpret_f32`, `i64.extend_i32_u` | `i32.wrap_i64`, `f32.reinterpret_i32` |
| `f64` → `i64` | `i64.reinterpret_f64` | `f64.reinterpret_i64` |

The zero fill is a choice of the reference implementation (N-D-47): the Canonical ABI ignores unused positions, but writing zero keeps the output independent of stale register contents and therefore deterministic.

### 7.3.3 Core signatures

For a WIT function type with parameters `p1 … pn` and result `r` (absent for a function without result), the core signature depends on the direction:

```
MAX_FLAT_PARAMS  = 16
MAX_FLAT_RESULTS = 1

core_signature(ft, direction):          // direction: Export (lifted) or Import (lowered)
    params  := flat(p1) ++ … ++ flat(pn)
    if len(params) > MAX_FLAT_PARAMS:
        params := [i32]                 // pointer to a tuple<p1, …, pn> in linear memory
    results := r absent ? [] : flat(r)
    if len(results) > MAX_FLAT_RESULTS:
        match direction:
            Export: results := [i32]                   // the export returns a pointer to its return area
            Import: params.push(i32); results := []    // the caller passes a return pointer
    return (params, results)
```

- A **constructor** `[constructor]r` has the result `own<r>`.
- A **method** `[method]r.m` has an implicit first parameter `self: borrow<r>`, counted in `params`.
- A **static function** `[static]r.s` has no implicit parameter.
- The **spilled parameter tuple** is laid out as `record { 0: p1, …, n-1: pn }` (§7.2.2). For an export, the caller (the runtime) allocates it through `cabi_realloc` and the glue frees it (§7.5.4). For an import, the glue builds it in its own frame (§7.7.2).
- The **return area** has the layout of `r`. For an export it is the static return area of §7.5.3; for an import it is a slot in the glue's frame.

Resource intrinsics have fixed core types: `[resource-new]r: (i32) -> i32` (rep to handle), `[resource-rep]r: (i32) -> i32` (handle to rep), `[resource-drop]r: (i32) -> ()`, and the destructor export `[dtor]r: (i32) -> ()` (rep).

### 7.3.4 Examples

For the `processor` interface of [spec/13 §13.3.3](../spec/13-components-and-wit.md), exported from `acme:imaging@0.3.0`:

| Function | WIT type | Core signature |
|---|---|---|
| `resize` | `func(image: list<u8>, width: u32, height: u32) -> result<list<u8>, image-error>` | `(param i32 i32 i32 i32) (result i32)`; the result `[i32, i32, i32]` is spilled |
| `[constructor]image` | `func(data: list<u8>) -> own<image>` | `(param i32 i32) (result i32)` |
| `[method]image.width` | `func(self: borrow<image>) -> u32` | `(param i32) (result i32)` |
| `[dtor]image` | — | `(param i32)` |

The result of `resize` is `variant { ok(list<u8>), error(image-error) }`: the `ok` payload flattens to `[i32, i32]`, the `error` payload (an enum) to `[i32]`, so `joined` is `[i32, i32]` and the whole result is `[i32, i32, i32]`. In memory it has a `u8` discriminant at offset 0 and the payload at offset 4: size 12, alignment 4.

For the imported method `[method]output-stream.blocking-write-and-flush` of `wasi:io/streams@0.2.3`, `func(self: borrow<output-stream>, contents: list<u8>) -> result<_, stream-error>`, where `stream-error` is `variant { last-operation-failed(own<error>), closed }`: the parameters flatten to `[i32, i32, i32]`; the result flattens to `[i32] ++ joined(stream-error) = [i32, i32, i32]`, which exceeds `MAX_FLAT_RESULTS`, so the core import is `(param i32 i32 i32 i32)` with the return pointer last and no result. The return area holds a `result` of size 12 and alignment 4: discriminant at 0, `stream-error` at 4 (its own discriminant at 4, the `error` handle at 8).

---

## 7.4 Lifting and lowering values

### 7.4.1 Directions

This document uses the guest's point of view:

- **lower** converts a SAI value into its Canonical ABI form: core values (`lower_flat`) or bytes at an address (`store`). Glue lowers the arguments of import calls and the results of exports.
- **lift** converts a Canonical ABI form into a SAI value: from core values (`lift_flat`) or from bytes at an address (`load`). Glue lifts the results of import calls and the parameters of exports.

The runtime performs the opposite operation on its side of every crossing. Every value that the runtime lowers into a SAI instance has already been validated by the runtime's lift on the other side (§7.12), so glue does not validate values it lifts (N-D-48). The only adjustments glue makes are the representation adjustments that the Canonical ABI itself defines for lifting (`bool` is "non-zero", narrow integers are truncated); they bring every value into the canonical form of [06 §6.5.2](06-wasm-lowering-and-layout.md) at the cost of at most one instruction.

The SAI type on the glue's side is the mapped type of [spec/13 §13.9](../spec/13-components-and-wit.md) in the position where the value appears: a WIT `string` is `String` or, in a top-level parameter, possibly `Str`; a `list<t>` is `Vec<T>`, `Bytes` or, in a top-level parameter, possibly `Slice<T>`.

### 7.4.2 Scalars

| WIT type | SAI type | `lower_flat` | `lift_flat` | `store` | `load` |
|---|---|---|---|---|---|
| `bool` | `Bool` | the `i32` (already 0 or 1) | `x != 0` (`i32.ne` with 0) | `i32.store8` | `i32.load8_u`, then `!= 0` |
| `s8` | `I8` | the `i32` | `i32.extend8_s` | `i32.store8` | `i32.load8_s` |
| `u8` | `U8`, `Byte` | the `i32` | `i32.and 0xFF` | `i32.store8` | `i32.load8_u` |
| `s16` | `I16` | the `i32` | `i32.extend16_s` | `i32.store16` | `i32.load16_s` |
| `u16` | `U16` | the `i32` | `i32.and 0xFFFF` | `i32.store16` | `i32.load16_u` |
| `s32`, `u32` | `I32`, `U32` | identity | identity | `i32.store` | `i32.load` |
| `s64`, `u64` | `I64`, `U64` | identity | identity | `i64.store` | `i64.load` |
| `f32` | `F32` | identity | identity | `f32.store` | `f32.load` |
| `f64` | `F64` | identity | identity | `f64.store` | `f64.load` |
| `char` | `Char` | identity | identity | `i32.store` | `i32.load` |

All memory accesses are little-endian and use the natural alignment as the alignment hint. NaN payloads are not preserved: the Canonical ABI permits the runtime to replace any NaN by a NaN of its choice ([spec/13 §13.9.1](../spec/13-components-and-wit.md)); glue itself never changes a float's bits.

### 7.4.3 Strings

`string` is UTF-8 in both representations, so strings are never transcoded or copied by glue.

| Situation | SAI type | Conversion |
|---|---|---|
| lower (import argument, export result) | `String` | `(ptr, len)` of the `String`; `cap` is not passed |
| lower (import argument) | `Str` | `(ptr, len)` of the view |
| lift (import result, export parameter) | `String` | `String { ptr, len, cap: len }`; the buffer was allocated by the runtime through `cabi_realloc` with alignment 1 and size `len`, so the `String` owns it |
| lift (export parameter) | `Str` | `Str { ptr, len }`; the glue frees the buffer after the call returns (§7.5.4) |

When `len = 0`, a lift ignores the incoming pointer and uses the empty representation (N-D-63) of [06 §6.4.4–6.4.5](06-wasm-lowering-and-layout.md) (`ptr = align(T)`, N-F-6, which is 1 for the bytes of `String` and `Str`; `cap = 0`); nothing is freed for it, because an allocation of size 0 never reserves memory (§7.5.1). A lowered empty `String` or `Str` passes its own pointer (`align(T)` = 1) and length 0, which the Canonical ABI accepts because a zero-length string imposes no bounds requirement beyond the pointer itself.

### 7.4.4 Lists

A list of element type `t` mapped to SAI element type `T` is **zero-copy** if `cabi_compatible(T, t)` holds (§7.9.2): then SAI's element layout and the Canonical ABI element layout are identical, byte for byte.

| Situation | SAI type | Zero-copy | Not zero-copy |
|---|---|---|---|
| lower | `Vec<T>`, `Bytes` | `(ptr, len)` of the vector | allocate a buffer of `len · size(t)` bytes with alignment `align(t)`, `store` element `k` at offset `k · size(t)` for `k = 0 … len-1`, pass `(buffer, len)`; the buffer is freed after the call (imports) or by post-return (exports) |
| lower | `Slice<T>` | `(ptr, len)` of the view | as above |
| lift | `Vec<T>`, `Bytes` | `Vec { ptr, len, cap: len }` taking over the runtime's buffer | allocate a `Vec<T>` with capacity `len`, `load` element `k` from offset `k · size(t)` and push it, then free the runtime's buffer (`len · size(t)` bytes, alignment `align(t)`) |
| lift | `Slice<T>` (export parameter) | `Slice { ptr, len }`; the buffer is freed after the call | build a temporary `Vec<T>` as above, pass a view of it, drop the `Vec<T>` after the call |

- Element conversion is recursive: an element that contains a `string` is stored as `(ptr, len)` of the SAI `String` (zero-copy at the string level, even when the list itself is converted), and a nested `list` is converted by the same table.
- When `len = 0`, lift uses the empty representation (`ptr = align(T)`, `cap = 0`) and frees nothing; lowering a converted empty list passes the pointer `align(t)` and allocates nothing.
- Element `k` is converted in increasing order of `k`. Buffers allocated while lowering one value are allocated in the order in which their lists are first reached in a depth-first, left-to-right traversal of the value, and freed in the reverse order (§7.5.4).

### 7.4.5 Records and tuples

A record or tuple is lowered field by field in **WIT declaration order**: `lower_flat` concatenates the flattenings of its fields; `store` stores field `i` at `offset(fi)` of §7.2.2. The SAI value is accessed through field projections, so SAI's internal field order ([06 §6.4.6](06-wasm-lowering-and-layout.md)) never affects the result. Lifting constructs the SAI value field by field in the same order. A record whose type is `cabi_compatible` with its SAI type is copied with one `memory.copy` of `size(record)` bytes when it is loaded or stored as a whole (an optimization that produces the same bytes).

### 7.4.6 Variants, enums, options and results

A SAI enum mapped to a WIT variant (including `enum`, `option` and `result`, after despecialization, §7.2.1) is converted by case:

```
lower_flat_variant(v: SAI enum value, t: variant { c0, …, c(n-1) }):
    match v:                                    // a MIR SwitchInt on the SAI discriminant
        case i with payload x:
            out := [i32.const i] ++ coerce_into_joined(lower_flat(payload_type(ci), x))   // §7.3.2
        case i without payload:
            out := [i32.const i] ++ zeros(joined(t))

lift_flat_variant(vals, t):
    d := vals[0]
    switch d:                                   // br_table over 0 … n-1
        case i: construct SAI case i from coerce_out_of_joined(vals[1 …], flat(payload_type(ci)))
        default: unreachable                    // never taken (N-D-48): the runtime lowers only valid discriminants

store_variant(v, t, addr):
    store the discriminant i as an integer of size(disc(n)) bytes at addr
    if case i has a payload: store(payload_type(ci), x, addr + payload_offset(t))

load_variant(t, addr):
    d := load an unsigned integer of size(disc(n)) bytes at addr
    switch d: case i: construct SAI case i from load(payload_type(ci), addr + payload_offset(t))
```

The case index `i` is the WIT declaration index:

| SAI type | WIT type | Case indices |
|---|---|---|
| `Option<T>` | `option<t>` | `None` → 0, `Some(x)` → 1 |
| `Result<T, E>` | `result<t, e>` | `Ok(x)` → 0, `Err(e)` → 1; a `()` arm has no payload |
| interface enum `E` | `enum` | variant `k` in declaration order → `k` |
| interface variant `V` | `variant` | case `k` in declaration order → `k` |

SAI's own discriminant encoding (tag, niche filling, [06 §6.4.12](06-wasm-lowering-and-layout.md)) is read and written only through MIR's discriminant operations, so it never needs to agree with the Canonical ABI. The `default` arm of `lift_flat_variant` is required by the structure of `br_table`; it lowers to `unreachable`.

### 7.4.7 Flags

A SAI flags type has the internal layout of an unsigned integer of `size(flags)` bytes with bit `i` for flag `i` (§7.9.1 and [06](06-wasm-lowering-and-layout.md) §6.4.18), so `lower_flat` is the value zero-extended to `i32`, `lift_flat` is the `i32` truncated to `size(flags)` bytes (`i32.and 0xFF` or `0xFFFF` for 1 or 2 bytes), and `store` and `load` are integer stores and unsigned loads of `size(flags)` bytes.

### 7.4.8 Handles

Owned and borrowed handles are converted by the rules of §7.6, which depend on whether the resource is imported or implemented by the component.

---

## 7.5 Boundary memory: `cabi_realloc`, return areas and post-return

### 7.5.1 `cabi_realloc`

`cabi_realloc` is the Canonical ABI `realloc` option of every lifted and lowered function that needs one. It is the runtime function of [10-runtime.md](10-runtime.md) §10.3, built on the program's allocator (D-WASM-05); this section fixes the contract that glue and the runtime rely on.

```
cabi_realloc(old_ptr: i32, old_size: i32, align: i32, new_size: i32) -> i32
```

| Arguments | Result |
|---|---|
| `align` not in {1, 2, 4, 8} | trap (`unreachable`); the Canonical ABI never passes another alignment for the types of §7.2 |
| `old_size = 0`, `new_size = 0` | `align`; nothing is allocated |
| `old_size = 0`, `new_size > 0` | a fresh allocation of `new_size` bytes aligned to `align` (`old_ptr` is ignored) |
| `old_size > 0`, `new_size = 0` | `old_ptr` is freed (`old_size`, `align`); the result is `align` |
| `old_size > 0`, `new_size > 0` | a reallocation that preserves the first `min(old_size, new_size)` bytes; the result may differ from `old_ptr` |

- An allocation that cannot be satisfied follows the out-of-memory path of [10-runtime.md](10-runtime.md) §10.2.6, which ends in a trap. `cabi_realloc` never returns 0. A result for `new_size > 0` is a heap block and is never below 8 ([10-runtime.md](10-runtime.md) §10.3.1); a result for `new_size = 0` is `align` itself (1, 2, 4 or 8), which the Canonical ABI never dereferences because the buffer is empty.
- Memory obtained from `cabi_realloc` is ordinary heap memory: a buffer of `size` bytes with alignment `align` is freed with the allocator's deallocation of the same size and alignment, which is exactly what `String`, `Vec<T>` and `Box<T>` do when they own the buffer. This is what makes the zero-copy lifts of §7.4.3 and §7.4.4 sound.
- The runtime calls `cabi_realloc` only while one of the component's exports is running or while one of its imports is being called; it is never reentered, because it calls no import.

`cabi_realloc` is exported under that name exactly when the component type needs it (N-D-49):

- some export's parameters contain a `string` or a `list`, or flatten to more than `MAX_FLAT_PARAMS` values; or
- some imported function in `Imp` that is reachable, and is therefore a core import (§7.7.1), has a result that contains a `string` or a `list`.

Each of these conditions is also one of the conditions under which [spec/08 §8.14.3](../spec/08-effects-and-capabilities.md) makes `alloc` reachable through the boundary, so a component that exports `cabi_realloc` always links the allocator and has passed the `alloc` capability check. The converse does not hold: glue may allocate with the allocator directly (converted lists, boxes of exported resources) without the component type needing `cabi_realloc`. A component that needs no `cabi_realloc` does not export one, and its canonical functions carry no `realloc` option.

### 7.5.2 Ownership of boundary memory

| Memory | Allocated by | Owned after the crossing by | Freed by |
|---|---|---|---|
| `string` / `list` buffers of export parameters | runtime, through `cabi_realloc` | the lifted `String`, `Vec<T>` or `Bytes` | normal SAI drop of that value |
| … lifted as `Str` or `Slice<T>` (zero-copy) | runtime, through `cabi_realloc` | the export glue | export glue, after the call (§7.8.2) |
| … of a converted list | runtime, through `cabi_realloc` | the export glue | export glue, immediately after conversion |
| spilled parameter area of an export | runtime, through `cabi_realloc` | the export glue | export glue, after lifting the parameters |
| buffers in an export result (zero-copy) | SAI code | the pending result (§7.5.3) | post-return (§7.5.5) |
| converted list buffers in an export result | export glue | the export glue | post-return |
| return area of an export | static (§7.5.3) | — | never |
| buffers of import arguments (zero-copy) | SAI code | the SAI argument | the caller's code as usual for borrowed arguments; the release walk (§7.5.4) for owned arguments |
| converted list buffers of import arguments | import glue | the import glue | import glue, after the call |
| spilled parameter tuple and return area of an import | import glue's frame | — | frame exit |
| `string` / `list` buffers of import results | runtime, through `cabi_realloc` | the lifted SAI value | normal SAI drop; converted lists: import glue, after conversion |

No memory crosses the boundary in the other direction: the runtime copies every value out of the component's memory before the component can free it, and the component never retains a pointer into memory it does not own.

### 7.5.3 Component runtime state

Component lowering contributes up to three runtime-state items. Each is emitted only if used, after the runtime state items of [10-runtime.md](10-runtime.md), in the order below, and placed by [06 §6.3.6](06-wasm-lowering-and-layout.md). They are zero-initialized and are written only by glue (N-D-50).

| Item | Size and alignment | Purpose |
|---|---|---|
| `__cabi_ret_area` | the maximum of `size(r)` and of `align(r)` over all exports, and all provided functions (§7.11.10), whose results are spilled | the return area of every export (§7.3.3) |
| `__cabi_pending` | the maximum SAI size and alignment ([06](06-wasm-lowering-and-layout.md)) of the result type over all exports, and all provided functions, that have a post-return function | holds the SAI result value between the export's return and its post-return |
| `__cabi_consumed_rep` | `PS` bytes, alignment `PS` | the rep of an exported resource whose value has just been moved out (§7.6.3) |

One area of each kind suffices: the Component Model does not let any caller enter an instance while one of its exports is running or before its post-return function has run, so at most one export result is in flight at a time. The provision glue of §7.11.10 uses the same areas: a provided call, including its post-return function, completes inside the glue that makes it, before the caller writes any result of its own, so provided calls nest strictly inside one another and inside exports, and no area is overwritten while a value in it is still needed. Async exports (Stage 5) do not use these areas; their results are delivered with `task.return`, which is outside the scope of v0.1.

### 7.5.4 The release walk and the free walk

Two type-directed walks are generated as synthetic functions, memoized per type:

- **`release(T, place)`** ends the life of a SAI value whose content has been transferred to the other side. It frees the heap buffers the value owns (`String`, `Vec<T>`, `Bytes`: first `release` each element if the element type owns anything, then free `cap · size(T)` bytes with alignment `align(T)`), recursing through struct fields, tuple elements and the active payload of an enum. It does **nothing** for resource values, imported or exported, because their ownership has moved through the handle (§7.6), and it runs no `Drop` impl (N-D-54). Skipping `Drop` is unobservable, because `Drop` is pure (D-OWN-05) and freeing memory is not an effect.
- **`free_converted(t, cabi value)`** frees the buffers that glue allocated for converted lists inside the Canonical ABI representation of a value of WIT type `t`: for each converted `list<u>` (`cabi_compatible` false), it first applies `free_converted(u, ·)` to each element in increasing index order, then frees `len · size(u)` bytes with alignment `align(u)`. Zero-copy lists and strings are skipped; their buffers belong to a SAI value.

Both walks are total, allocate nothing, and call no import.

### 7.5.5 Post-return functions

An export whose WIT result type contains a `string` or a `list` has a post-return function; no other export has one (N-D-51). Its core export name is `cabi_post_` followed by the export's core name (§7.10.2), and its core type is `(param <flat results of the export, after spilling>)`, which is always `(param i32)`, the address of `__cabi_ret_area`, because a result that contains a `string` or a `list` flattens to at least two values.

```
cabi_post_<name>(ret: i32):
    free_converted(r, the value of type r at ret)       // §7.5.4
    release(R, __cabi_pending)                           // R: the SAI result type
```

The runtime calls the post-return function after it has copied the result out of linear memory and before any other call into the instance. Post-return functions have no effects; they are not roots of the effect analysis, because they only free memory.

---

## 7.6 Resources and handles

### 7.6.1 Two kinds of resources

| Kind | Example | SAI value | Canonical ABI rep |
|---|---|---|---|
| **imported** resource: defined by an imported interface | `streams.OutputStream` | the handle, an `i32` index into the instance's resource table ([06 §6.4.14](06-wasm-lowering-and-layout.md)) | owned by the other component or the host; never visible |
| **exported** resource: an abstract interface resource bound with `type R = C` in an impl of an exported interface | `processor.Image` bound to `MyImage` | the value of the concrete type `C`, laid out as a struct ([06 §6.4.14](06-wasm-lowering-and-layout.md)) | a `Box<C>` pointer: the address of a heap allocation that holds the `C` value (D-WASM-03) |

The Component Model gives every instance one resource table. Handles held by SAI code are always handles of imported resources. The component never holds handles of its own exported resources: inside the component such a resource is a plain `C` value, and it becomes a handle only at the moment it crosses the boundary.

A resource type exported by the component never appears in the signature of an imported function: an interface is never both imported and exported ([spec/13 N-D-30](../spec/13-components-and-wit.md)), and a valid WIT world cannot make an imported interface depend on the types of an exported one. The glue generator treats such a signature as an internal compiler error.

A resource of an interface provided within the build (§7.11.10) is of both kinds at once: its callers hold it as an imported resource, and its provider implements it as an exported resource. Its handles are indices into the in-module handle table, not into the instance's resource table (§7.6.5).

### 7.6.2 Imported resources

| Operation | Glue |
|---|---|
| pass `R` (owned) to an import | lower the handle as is; the SAI argument has been moved into the call and is not dropped |
| receive `R` (owned) from an import | the lifted handle is the SAI value |
| pass `borrow R` to an import | load the handle through the borrow and lower it; the runtime lends it for the duration of the call |
| export receives `R` (owned) | the lifted handle is the SAI value (the runtime has moved it into this instance's table) |
| export receives `borrow R` | the runtime has created a borrowed handle `h` in this instance's table: store `h` in a frame slot of type `R`, pass the slot's address as `borrow R`, and call `[resource-drop]r(h)` after the implementation returns and before the export returns (§7.8.2) |
| export returns `R` (owned) | lower the handle; ownership moves to the caller |
| drop of an owned `R` | the drop glue of `R` calls `[resource-drop]r(h)`, imported from the resource's interface module (§7.10.1) |

The drop glue of an imported resource type is the only drop glue that calls an import. It has no effect ([spec/08](../spec/08-effects-and-capabilities.md), D-OWN-05): the Component Model may run the exporter's destructor, but that is the exporter's behaviour, not an observable effect of the SAI program. A borrowed handle is never dropped by drop glue; the slot that holds it in an export is not an owned `R` for drop elaboration, because glue is generated after drop elaboration (MIR phase `Mono`).

### 7.6.3 Exported resources

For each exported resource `r` of exported interface `ns:pkg/iface@v`, bound to the concrete type `C`, component lowering emits:

- three core imports from the module `[export]ns:pkg/iface@v`: `[resource-new]r`, `[resource-rep]r` and `[resource-drop]r` (§7.3.3), each only if used;
- one core export `ns:pkg/iface@v#[dtor]r`, the destructor, always.

The rep is a plain `Box<C>` allocation; whether a destructor call must drop the value is decided by the single slot `__cabi_consumed_rep` rather than by a state word in each allocation (N-D-53).

The operations are:

```
lower own (an export returns C, including constructors):
    p := allocate(size(C), align(C))            // the allocator of impl/10; align(C) itself if size(C) = 0
    move the C value into *p
    h := [resource-new]r(p)
    lower h

lift own (an export receives C by value):
    p := [resource-rep]r(h)
    v := move *p                                 // a memory copy of size(C) bytes into the parameter
    __cabi_consumed_rep := p
    [resource-drop]r(h)                          // the runtime calls [dtor]r(p) synchronously (below)
    the SAI parameter is v

lift borrow (an export receives borrow C or borrow mut C, including method receivers):
    the runtime passes the rep p itself, not a handle
    the SAI parameter is p, used directly as borrow C (06 §6.4.14)

[dtor]r(p):
    if p == __cabi_consumed_rep:
        __cabi_consumed_rep := 0                 // the value was moved out: do not drop it
    else:
        drop the C value at *p                   // its Drop impl and the drop glue of its fields
    free p (size(C), align(C))
```

- When the client drops its last owned handle, the runtime calls `[dtor]r(p)`, which drops the value. When the instance is torn down, no destructor runs ([spec/13 §13.10.4](../spec/13-components-and-wit.md)).
- `[resource-drop]r` on an owned handle of a resource implemented by the calling instance calls the destructor synchronously, before it returns, so `__cabi_consumed_rep` is set for exactly the one destructor call that follows. A rep is never 0, so the reset value 0 never matches.
- A zero-sized `C` has the rep `align(C)` for every value. This is valid: the Component Model does not require reps to be distinct, and the consumed check is still exact because only one destructor runs between setting and resetting the slot.
- `borrow mut C` received by an export is unique for the duration of the call because the Component Model forbids reentrance ([spec/13 §13.10.2](../spec/13-components-and-wit.md)).
- The runtime traps if a client passes a handle that is not a live handle of `r`, or drops an owned handle while it is lent; glue does not check either.

### 7.6.4 Handles inside aggregates

Handles inside records, variants, options, results and lists are converted element by element by the same rules. A `list` of owned handles of an imported resource is zero-copy (§7.9.2); a list that contains exported resources is always converted, because the SAI element is the `C` value and the Canonical ABI element is a handle. After an import call, the owned argument is released with `release` (§7.5.4), which skips every handle it contains.

### 7.6.5 Resources provided within the build

A resource `r` of an interface `ns:pkg/iface@v` provided within the build (§7.11.10) keeps the caller's glue of §7.6.2 and the provider's glue of §7.6.3 unchanged, except that its handles are indices into the in-module handle table of [10](10-runtime.md) §10.3.3 (N-D-108). Every operation that the Component Model would perform on the resource table for `r` is performed by a runtime function of that table, either directly or through the adapter of §7.11.10. Let `k` be the type index of `r` in the provision plan:

| Canonical ABI operation | In-module replacement |
|---|---|
| `[resource-new]r(p)` called by the provider | `__sai_handle_new(k, p)` |
| `[resource-rep]r(h)` called by the provider | `__sai_handle_rep(k, h)` |
| `[resource-drop]r(h)` called by the provider, or by the drop glue of `R` in a caller | the drop adapter `glue:[adapter]ns:pkg/iface@v#[resource-drop]r`, which computes `p := __sai_handle_drop(k, h)` and then calls the provider's `[dtor]r(p)` |
| lift `own<r>` for the provider's export glue | none: the adapter passes `h` unchanged, and the provider's lift own of §7.6.3 checks and consumes it |
| lift `borrow<r>` for the provider's export glue | `p := __sai_handle_lend(k, h)`, pass `p`; after the call returns, `__sai_handle_unlend(h)` |
| lift `borrow<r>` for a provided function of another provider | `__sai_handle_lend(k, h)`, pass `h` unchanged; after the call returns, `__sai_handle_unlend(h)` |
| lift `own<r>` for a provided function of another provider | `__sai_handle_move(k, h)`, pass `h` unchanged |
| `own<r>` in a result | passed unchanged |

- The runtime functions trap exactly where the Component Model's resource table traps: a handle that is not live, a handle of another resource type, an owned handle that is moved or dropped while it is lent, and a full table (N-D-108). An index that is a live handle of the instance's own resource table is not a live handle of the in-module table, and the reverse, because the two tables never exchange handles (N-D-105).
- A handle of a resource that the build does not provide (a WASI resource, a resource of a composed artifact, or one that remains an import) is passed through provision glue unchanged and without checks, because it lives in the instance's resource table, which provision glue does not modify. A borrowed handle of such a resource is not dropped by the provider's export glue (§7.11.10), because no new borrowed handle is created.
- The provider's `[dtor]r` runs synchronously inside the drop adapter, so `__cabi_consumed_rep` (§7.5.3) works as in §7.6.3. When the instance is torn down, no destructor runs, as for every resource ([spec/13 §13.10.4](../spec/13-components-and-wit.md)).

---

## 7.7 Import glue

### 7.7.1 Core imports

Every imported function in `Imp` that is reachable ([impl/08](08-effect-and-capability-analysis.md)) becomes exactly one core import with the module and field names of §7.10.1 and the core type `core_signature(ft, Import)` (§7.3.3). An imported function that is not reachable is not imported, even if its interface is in `Imp` because of types in exported signatures. The `[resource-drop]r` intrinsic of an imported resource `r` is imported when the drop glue of `r` is reachable or when an export receives `borrow R`. When the interface of `r` is not in `Imp`, no value of `r` can exist at run time, because every source of a handle (an imported function's result, an export's parameter, a type used by either) puts the interface in `Imp` ([spec/13 §13.5.2](../spec/13-components-and-wit.md) rules 1–3); its drop glue imports no intrinsic, consists of `unreachable`, and impl/08 §8.7.3 excludes it from the live resource drops. A function or a `[resource-drop]r` intrinsic of an interface provided within the build is never a core import: its import glue calls an adapter instead (§7.11.10).

### 7.7.2 Glue body

SAI code calls an imported function through its glue function, an ordinary function with the SAI view signature ([spec/13 §13.9.2](../spec/13-components-and-wit.md)) and the internal calling convention ([06 §6.6](06-wasm-lowering-and-layout.md)). SAI code calls an imported function `d` as `Const(Instance(Item(d)))`; [06](06-wasm-lowering-and-layout.md) §6.8.9 lowers that call to the import glue of `d`, whose symbol is `glue:<module>#<field>` ([06](06-wasm-lowering-and-layout.md) §6.10.2). The glue body is:

```
import_glue_f(a1: T1, …, an: Tn) -> R:
    // 1. Lower the arguments, in parameter order.
    if the parameters are not spilled:
        vals := lower_flat(p1, a1) ++ … ++ lower_flat(pn, an)
    else:
        P := a frame slot of size(tuple<p1, …, pn>), alignment align(tuple<…>)
        for i in 1 … n: store(pi, ai, P + offset_i)
        vals := [P]
    if the result is spilled:
        A := a frame slot of size(r), alignment align(r)
        vals.push(A)

    // 2. Call.
    out := Call Const(Instance(CoreImport { module, field, sig }))(vals)   // [04] §4.13.9, N-E-61

    // 3. Free what glue allocated for the arguments, in reverse parameter order.
    for i in n … 1: free_converted(pi, the lowered form of ai)          // §7.5.4

    // 4. Release owned arguments, in reverse parameter order.
    for i in n … 1 where Ti is an owned type: release(Ti, ai)           // §7.5.4

    // 5. Lift the result.
    if the result is spilled: return load(r, A)
    else:                     return lift_flat(r, out)
```

- In step 2, `module` and `field` are those of §7.10.1 and `sig` is `core_signature(ft, Import)` of §7.3.3, with the core types `i32`, `i64`, `f32`, `f64` and `ptr` as `I32`, `I64`, `F32`, `F64` and `USize`. The resource intrinsics of §7.6 (`[resource-drop]`, `[resource-new]` and `[resource-rep]`) are called in the same way.
- Borrowed arguments (`Str`, `Slice<T>`, `borrow R`) are neither released nor dropped; they belong to the caller.
- The lowered form of a non-spilled argument is kept in Wasm locals for step 3; the lowered form of a spilled argument is read back from `P`.
- The glue has the effects that [spec/08 §8.7.3](../spec/08-effects-and-capabilities.md) assigns to the call; effect analysis attributes them to the call site in the caller, not to the glue body, because glue is generated after the analysis (MIR phase `Mono`).
- A trap inside the call (in the runtime or in the callee) ends the instance; the glue has no cleanup path (D-ERR-01).
- Glue functions are ordinary candidates for inlining ([09](09-optimization-and-size.md)); the core import itself is never removed while the glue is reachable.
- Import glue calls no instance that takes a caller location: its failures are traps (§7.12), and the release walk runs no `Drop` impl (N-D-54). It sets the `SourceInfo.span` of its statements to the declaration of the imported function.

### 7.7.3 Example

The view function `streams.OutputStream.blocking_write_and_flush(self: borrow Self, contents: Slice<Byte>) -> Result<(), streams.StreamError>` (the import of §7.3.4) has the glue:

```
glue(self: borrow OutputStream, contents: Slice<Byte>) -> Result<(), StreamError>:
    h   := load i32 at self                                  // the handle
    A   := frame slot, 12 bytes, alignment 4
    call "wasi:io/streams@0.2.3" "[method]output-stream.blocking-write-and-flush"
         (h, contents.ptr, contents.len, A)                  // list<u8> is zero-copy
    d := i32.load8_u A
    if d == 0: return Ok(())
    // d == 1
    e := i32.load8_u (A + 4)
    if e == 0: return Err(StreamError.LastOperationFailed(i32.load (A + 8)))   // owned error handle
    return Err(StreamError.Closed)
```

Nothing is freed after the call: the only list argument is zero-copy and borrowed, and the result contains no `string` or `list`. The call therefore does not allocate at the boundary, and its effect set is `{ wasi:io/streams }` ([spec/08 §8.14.3](../spec/08-effects-and-capabilities.md)).

---

## 7.8 Export glue

### 7.8.1 Core exports

Every export of `W'` becomes exactly one core export: every function of every exported interface, every constructor, method and static function of every exported resource, every function exported directly by the world, the destructor of every exported resource (§7.6.3) and, for a command, `wasi:cli/run` (§7.8.4). Names are those of §7.10.2. The core type is `core_signature(ft, Export)` (§7.3.3). The exported core function is the export glue; the SAI function that implements the export (the impl function or the inherent function of the bound type, [spec/13 §13.3](../spec/13-components-and-wit.md)) is an ordinary internal function called by the glue.

### 7.8.2 Glue body

```
export_glue_f(core parameters) -> core results:
    // 1. Obtain the flat parameter values.
    if the parameters are spilled (the only core parameter is a pointer P):
        values are loaded from P: a_i := load(p_i, P + offset_i) for i in 1 … n
        free P (size(tuple<p1, …, pn>), align(tuple<…>))
    else:
        a_i := lift_flat(p_i, the core parameters of p_i) for i in 1 … n

    // 2. Boundary checks and call (D-CONTRACT-01).
    v := BoundaryEntry(g)(a_1, …, a_n)   // [04] §4.12.5: invariants of the parameters, then requires clauses (spec/09 §9.7.3); a violation panics

    // 3. Clean up parameters, in reverse parameter order.
    for i in n … 1:
        if the parameter is Str or a zero-copy Slice<T>: free its buffer (len · size(t), align(t)) if len > 0
        if the parameter is a converted Slice<T>:        drop the temporary Vec<T>
        if the parameter is borrow R of an imported resource: [resource-drop]r(h_i)

    // 4. Lower the result.
    if the export has a post-return function:               // the result contains a string or list
        move v into __cabi_pending
        store(r, __cabi_pending, __cabi_ret_area)           // zero-copy where possible, converted lists allocated
        return __cabi_ret_area
    if the result is spilled:
        store(r, v, __cabi_ret_area); release(R, v)         // release frees nothing here: v owns no buffer
        return __cabi_ret_area
    return lower_flat(r, v)                                 // or no result
```

- Step 1 lifts parameters in parameter order; within a parameter, in WIT field order (§7.4). Owned exported resources are consumed by the `lift own` sequence of §7.6.3 as they are lifted.
- Step 2 calls the internal function through its boundary shim. `BoundaryEntry(g)` is `g` itself when [04](04-mir.md) §4.12.5 creates no shim for `g`. The checks are defined by [spec/09 §9.7.3](../spec/09-contracts-and-refinements.md) and run in every profile. A failed check panics, which traps (D-ERR-01).
- Export glue is a compiler-generated body of [spec/10 §10.4.5](../spec/10-errors-and-panics.md). It sets the `SourceInfo.span` of its statements and terminators to the name of `g` in its declaration, so a panic in a transparent callee that it calls, such as the drop of the temporary `Vec<T>` of a converted `Slice<T>` in step 3, takes the position of the exported function ([04](04-mir.md) §4.13.7 item 2).
- A panic in `g` or in the glue traps after the panic report of [10-runtime.md](10-runtime.md) §10.5; it never becomes a WIT `result` and never unwinds ([spec/13 §13.13](../spec/13-components-and-wit.md)).
- The "borrowed handle not released" trap of the Component Model can therefore not occur for a terminating export: step 3 releases every borrowed handle the runtime created.
- An `async fn` export is `FEATURE_NOT_IMPLEMENTED` until Stage 5 (D-ASYNC-01).

### 7.8.3 Example

The export `resize` of §7.3.4, implemented by `processor.resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, processor.ImageError>`:

```
"acme:imaging/processor@0.3.0#resize"(ptr: i32, len: i32, w: i32, h: i32) -> i32:
    image := Bytes { ptr: len == 0 ? 1 : ptr, len, cap: len }      // takes over the runtime's buffer
    v := processor.resize(image, w, h)
    move v into __cabi_pending                                      // Result<Bytes, ImageError>
    match __cabi_pending:
        Ok(b):  i32.store8 (__cabi_ret_area + 0) 0
                i32.store  (__cabi_ret_area + 4) b.ptr              // zero-copy list<u8>
                i32.store  (__cabi_ret_area + 8) b.len
        Err(e): i32.store8 (__cabi_ret_area + 0) 1
                i32.store8 (__cabi_ret_area + 4) (case index of e)
    return __cabi_ret_area

"cabi_post_acme:imaging/processor@0.3.0#resize"(ret: i32):
    // free_converted: nothing (list<u8> is zero-copy)
    release(Result<Bytes, ImageError>, __cabi_pending)              // frees the Bytes buffer if Ok
```

The constructor of `image`, implemented by `MyImage.new(data: Bytes) -> MyImage`:

```
"acme:imaging/processor@0.3.0#[constructor]image"(ptr: i32, len: i32) -> i32:
    data := Bytes { ptr: len == 0 ? 1 : ptr, len, cap: len }
    v := MyImage.new(data)
    p := allocate(size(MyImage), align(MyImage)); move v into *p
    return "[export]acme:imaging/processor@0.3.0" "[resource-new]image" (p)
```

### 7.8.4 Commands: `wasi:cli/run`

A command ([spec/13 §13.5.3](../spec/13-components-and-wit.md)) exports `run: func() -> result` of `wasi:cli/run@V`, where `V` is the WASI version of the target ([targets](../targets/targets-and-profiles.md)). Its glue is:

```
"wasi:cli/run@V#run"() -> i32:
    env  := the process.Env value for this run          // constructed by the core library (16-core-library.md)
    code := main(env)
    return code is the success code ? 0 : 1             // result: ok = 0, error = 1
```

- The result `result` (no payloads) flattens to `[i32]`, so `run` has no return area and no post-return function.
- `process.Env` is constructed by the core library function that [16-core-library.md](../spec/16-core-library.md) designates as the entry-point constructor; the glue calls it and nothing else constructs a `process.Env`. That function and `main` are the callees of the `run` glue. The glue itself has an empty own set, so reachability ([spec/08 §8.12.3](../spec/08-effects-and-capabilities.md)) represents it by these callees, the `Main` and `EnvForEntry` roots, and by the drop glue of the result type of `main`, an edge of the `Main` root ([impl/08](08-effect-and-capability-analysis.md) §8.7.2 item 1, N-D-82); it is not a separate root.
- `ExitCode` carries no resources; it is dropped normally after the comparison, by the drop glue that reachability attaches to the `Main` root.
- The host maps `error` to a non-zero exit status of its choice; WASI 0.2 cannot transmit a specific code through `run` ([spec/13 N-D-31](../spec/13-components-and-wit.md)).

### 7.8.5 World-level exported functions

A function exported directly by a world (`export FnHead`, implemented by a root-module function, [spec/13 §13.4](../spec/13-components-and-wit.md)) has export glue exactly like an interface function. Its core export name is the WIT function name alone (§7.10.2).

---

## 7.9 Internal layout and the Canonical ABI

### 7.9.1 Differences

The internal layout of [06](06-wasm-lowering-and-layout.md) is compiler-private (D-WASM-03); the Canonical ABI is a fixed external format. They are kept separate, and glue converts between them at the boundary (N-D-52).

| Aspect | Internal layout ([06](06-wasm-lowering-and-layout.md)) | Canonical ABI | Conversion |
|---|---|---|---|
| record and tuple field order | sorted by decreasing alignment, ties in declaration order | declaration order, C-style padding | field by field (§7.4.5) |
| enum, variant, `Option`, `Result` | smallest tag, niche filling, no tag for a single present variant, absent variants | always a discriminant of `size(disc(n))` bytes, payload at `payload_offset` | case by case (§7.4.6) |
| `String`, `Vec<T>`, `Bytes` | `(ptr, len, cap)`, each `PS` bytes | `(ptr, len)`, each 4 bytes | drop `cap` when lowering; `cap = len` when lifting (§7.4.3–§7.4.4) |
| `Str`, `Slice<T>` | `(ptr, len)`, each `PS` bytes | `(ptr, len)`, each 4 bytes | identical on wasm32 |
| `Bool` | 1 byte, exactly 0 or 1 | 1 byte; lifting treats non-zero as true | `!= 0` when lifting |
| narrow integers in Wasm locals | canonical sign or zero extension ([06 §6.5.2](06-wasm-lowering-and-layout.md)) | the low bits of an `i32` | truncation when lifting |
| `Char` | 4 bytes, a scalar value | 4 bytes, a scalar value | identical |
| flags | unsigned integer of `size(flags)` bytes, bit `i` = flag `i` (requirement on [06](06-wasm-lowering-and-layout.md)) | the same | identical |
| imported resource | `i32` handle | `i32` handle | identical; ownership per §7.6.2 |
| exported resource | the `C` value itself | `i32` handle to a `Box<C>` rep | box and `resource-new`; `resource-rep` and move out (§7.6.3) |
| `borrow R` | pointer to the `R` value | `borrow<r>`: handle (imported) or rep (exported) | load the handle; the rep is the pointer |
| aggregates at calls | at most 4 leaves flattened, otherwise by address or sret (D-WASM-04) | at most 16 flat parameters and 1 flat result, otherwise through memory | glue adapts one to the other |
| empty buffers | `ptr = align(T)`, `cap = 0` | any aligned pointer, length 0 | normalized when lifting |

The only requirement this chapter places on [06](06-wasm-lowering-and-layout.md) is the flags row: a flags type ([spec/13 §13.9.4](../spec/13-components-and-wit.md)) MUST have the layout of `U8`, `U16` or `U32` (for at most 8, 16 or 32 flags) with bit `i` for flag `i`. [06](06-wasm-lowering-and-layout.md) §6.4.18 (N-F-90) states this layout. Everything else is converted.

### 7.9.2 Compatible element types

`cabi_compatible(T, t)` decides whether a list of SAI element type `T` can share its buffer with a Canonical ABI `list<t>`. It holds exactly when:

- `t` is `bool`, an integer type, `f32`, `f64` or `char`, and `T` is its mapped SAI type; or
- `t` is a `flags` type; or
- `t` is an `enum` with at least two cases and `layout(T)` is tagged with a tag of `size(disc(n))` bytes at offset 0 whose value for case `k` is `k`, and `size(T) = size(t)`; or
- `t` is `own<r>` of an imported resource; or
- `t` is a record or tuple, every field type is `cabi_compatible` with the corresponding SAI field type, every field has the same offset in `layout(T)` as in §7.2.2, and `size(T) = size(t)` and `align(T) = align(t)`.

In every other case (`string`, `list`, `variant`, `option`, `result`, `borrow<r>`, `own<r>` of an exported resource, a single-case `enum`, a record whose field order differs) it is false. The predicate is computed from the actual layouts of [06](06-wasm-lowering-and-layout.md), never assumed, so it remains correct when the internal layout changes (D-WASM-03). It affects only performance: a list for which it is false is converted (§7.4.4).

Because the predicate depends on the compiler-private layout, it never affects effects or capabilities: [spec/08 §8.14.3](../spec/08-effects-and-capabilities.md) uses the layout-independent notion "flat-primitive", which implies `cabi_compatible`, and treats every other list as potentially converted.

---

## 7.10 Core names

The names below are the core-module conventions that `wit-component` (D-TOOL-05) recognizes when it wraps a core module (its "legacy" name mangling); the reference implementation uses exactly these and no other mangling scheme (N-D-55). `@v` is `@` followed by the version of the WIT package as fixed by resolution ([spec/13 §13.11.4](../spec/13-components-and-wit.md)); it is omitted for a WIT package without version. Every name uses the WIT (kebab-case) form of [spec/13 §13.8](../spec/13-components-and-wit.md).

### 7.10.1 Core imports

| Item | Module | Field |
|---|---|---|
| function `f` of imported interface `ns:pkg/iface@v` | `ns:pkg/iface@v` | `f` |
| constructor of imported resource `r` | `ns:pkg/iface@v` | `[constructor]r` |
| method `m` of imported resource `r` | `ns:pkg/iface@v` | `[method]r.m` |
| static function `s` of imported resource `r` | `ns:pkg/iface@v` | `[static]r.s` |
| drop of imported resource `r` (owned or borrowed handle) | `ns:pkg/iface@v` | `[resource-drop]r` |
| function `f` imported directly by the world | `$root` | `f` |
| intrinsics of exported resource `r` of exported interface `ns:pkg/iface@v` | `[export]ns:pkg/iface@v` | `[resource-new]r`, `[resource-rep]r`, `[resource-drop]r` |

`ns:pkg/iface@v` is the interface in which the resource or function is defined, not an interface that merely uses it.

### 7.10.2 Core exports

| Item | Export name |
|---|---|
| function `f` of exported interface `ns:pkg/iface@v` | `ns:pkg/iface@v#f` |
| constructor, method, static function of exported resource `r` | `ns:pkg/iface@v#[constructor]r`, `ns:pkg/iface@v#[method]r.m`, `ns:pkg/iface@v#[static]r.s` |
| destructor of exported resource `r` | `ns:pkg/iface@v#[dtor]r` |
| function `f` exported directly by the world | `f` |
| `run` of a command | `wasi:cli/run@V#run` |
| post-return of export `E` | `cabi_post_E` |
| realloc | `cabi_realloc` |
| linear memory | `memory` |

Local interfaces use the root package's namespace, name and version (for example `acme:imaging/processor@0.3.0`).

### 7.10.3 Order of core imports

Core imports created by component lowering are emitted, before any defined function, in increasing byte-wise order of the pair (module name, field name) (N-D-60). Function indices are then assigned by [06](06-wasm-lowering-and-layout.md) in that order. The order is independent of the order in which calls were encountered, so adding a call never renumbers unrelated imports in a way that depends on source order (determinism principle).

### 7.10.4 Order of core exports

The export section lists all exports in increasing byte-wise order of their names (N-D-60). The memory export exists exactly when the module has a memory ([06 §6.3.7](06-wasm-lowering-and-layout.md)); component lowering reports to [06](06-wasm-lowering-and-layout.md) that the boundary needs a memory when the target is a WASI target or when any canonical function passes a `string`, a `list`, a spilled parameter tuple or a return area.

---

## 7.11 Component assembly

### 7.11.1 The `component-type` custom section

The core module carries the component's type as a custom section named `component-type:sai` (N-D-56). Its content is the encoding produced by `wit_component::metadata::encode` for the synthetic world of §7.11.2, with the string encoding `utf8` and the producers of §7.11.6. The section is the last section of the core module. `wit-component` consumes it when wrapping and does not copy it into the component.

### 7.11.2 The synthetic world

The world that describes the component is a synthetic world `root` in a synthetic WIT package `sai:root`, added to the `Resolve` of §7.1.1. It contains:

- `import` of every interface in `Imp` that is not provided within the build (§7.11.10), and of every directly imported world function that is reachable ([spec/13 §13.5.2](../spec/13-components-and-wit.md));
- `export` of every interface and function exported by `W'` (for a command, including `wasi:cli/run@V`).

Interfaces keep their real identities (`ns:pkg/iface@v`), so the import and export names of the component are those of the export world. The synthetic package and world names are not observable in the component: a component type contains only its imports and exports. Using a synthetic world rather than the export world itself (N-D-57) is what applies the pruning of [spec/13 N-D-41](../spec/13-components-and-wit.md) and the command extension of [spec/13 §13.5.3](../spec/13-components-and-wit.md).

### 7.11.3 Wrapping

The component is produced by `wit_component::ComponentEncoder` with the core module as its only module, validation enabled, and no adapter (WASI preview 1 is out of scope, D-TGT-01; N-D-61). `wit-component` creates the canonical `lower` of each core import, the canonical `lift` of each core export with its `realloc`, `memory`, `post-return` and `string-encoding=utf8` options, and the resource types and intrinsics of §7.6. The core module has no start function and no `_initialize` export, because SAI has no static initialization (D-CAP-02); data segments are applied when the instance is created.

### 7.11.4 Commands and reactors

The kind is decided by [spec/13 §13.5.3](../spec/13-components-and-wit.md). At this level the only difference is the export set: a command exports `wasi:cli/run@V#run` (§7.8.4) in addition to the exports of `W'`, and a reactor does not. Neither has any other entry point.

### 7.11.5 Components of core targets

`sai component build` for a core target ([spec/13 §13.6.2](../spec/13-components-and-wit.md)) runs the same pipeline with no interface imported: every entry of `Imp` is either provided within the build (§7.11.10) or `TARGET_CAPABILITY_UNAVAILABLE`, which analysis reports before lowering. The core module imports only the resource intrinsics of exported resources (module `[export]…`), which are canonical built-ins and not component imports, so the component has no imports. The memory export, `cabi_realloc` and post-return functions follow the same rules as on WASI targets. The result is always a reactor.

### 7.11.6 Custom sections and determinism

- The core module's own custom sections (name section, producers, DWARF) are those of the profile ([06](06-wasm-lowering-and-layout.md) §6.11–§6.12). `wit-component` keeps the core module, including its name section, inside the component.
- The producers information records `processed-by` entries for `sai` and for `wit-component` with their exact versions. It contains no timestamps, paths, user names or host names.
- The component's bytes are a function of the core module, the synthetic world and the pinned `wit-component` version. Two builds with the same inputs and the same toolchain MUST produce byte-identical components (§7.1.2).

### 7.11.7 `wasm64-wasi`

The Canonical ABI with 64-bit memories is not supported by the pinned versions of `wit-component` and `wasmtime`. In v0.1 the reference implementation therefore stops a component build for `wasm64-wasi` after the core module has been produced and validated, with `FEATURE_NOT_IMPLEMENTED` (N-D-58). Analysis, `sai check` and the core module itself are fully supported on that target ([06](06-wasm-lowering-and-layout.md)).

### 7.11.8 Panic reports in components

Component lowering never adds an import in order to report panics (N-D-59). When the panic reporting channel of [10-runtime.md](10-runtime.md) §10.5.3 is a WIT import such as `wasi:cli/stderr`, it is used only if that interface is already in `Imp`; otherwise a panic in a component traps without a report. Panic reporting therefore never changes the component's imports or the capabilities it requires, in agreement with panics having no effect ([spec/08](../spec/08-effects-and-capabilities.md)).

### 7.11.9 Composition with artifact dependencies

A component build whose active graph contains packages used in artifact form ([package/04](../package/04-distribution-and-security.md) §4.5.1) composes their artifacts with the component `C₀` produced by §7.11.3. This is the composition that package/04 §4.5.1 item 5 refers to. It happens at build time, so it is not dynamic linking ([targets](../targets/targets-and-profiles.md) §5.7 item 1). Only `wasm32-wasi` builds compose: an artifact package cannot be active in a build for a core or native target (package/04 §4.5.1), a hybrid package is used in source form there, and a component build for `wasm64-wasi` stops before wrapping (§7.11.7).

The inputs are `C₀` with its imports (the entries of `Imp` that are not provided within the build, §7.11.2), and the set `A` of the active nodes used in artifact form. A hybrid node used in source form is not in `A`: it is never composed, and it provides its interfaces within the build instead (§7.11.10), so they are not imports of `C₀`, and no member of `U` imports one of them ([spec/13](../spec/13-components-and-wit.md) §13.5.2, `PROVIDED_INTERFACE_AT_BOUNDARY`). Each node `n ∈ A` has its artifact for the target, verified by package/04 §4.5.2, and its export world `W_n` (package/04 §4.5.2 item 3). The component type of an artifact is read with `wasmparser` (D-TOOL-05). The **importers** are `C₀` and the members of `A`.

1. **Providers.** Let `I = ns:pkg/iface@v` be an import of an importer `m`. The **provider** of `I` for `m` is the node `n ∈ A` whose identity is `ns:pkg` and whose selected version is `v`, if all of the following hold:
   - `I` is not in the `wasi` namespace;
   - `W_n` exports `I`;
   - `m` is `C₀`, or `n` is a dependency of `m`, direct or indirect, in the active graph.

   Otherwise `I` has no provider for `m`. The WIT package of a node has the node's identity as its name and the node's version ([package/01](../package/01-package-contract.md) §1.6.3 item 3), and a graph has one node, with one selected version, per identity and compatibility line ([spec/13](../spec/13-components-and-wit.md) §13.7.1), so `n` is unique when it exists. Two lines of one package in `A` are two different providers, each for the imports that carry its version. An import of the `wasi` namespace is always supplied by the host. The exports of `C₀` never provide an import. Because the active graph is acyclic ([package/02](../package/02-resolver.md) §2.4.4), the provider relation is acyclic too (N-D-102).
2. **Used set.** `U` is the least set that contains the provider of every import of `C₀` that has one, and the provider of every import of a member of `U` that has one. A node of `A` outside `U` is not composed, because nothing in the build can call it: SAI code, including the source of every package that depends on an artifact package, reaches an artifact only through the imports of `C₀` (package/04 §4.5.3 item 1). The imports of such a node are still required capabilities ([impl/08](08-effect-and-capability-analysis.md) §8.7.5 item 2, D-CAP-01).
3. **Outer imports.** The **outer imports** are the imports of `C₀` and of the members of `U` that have no provider for their importer, each name once, ordered by the bytes of the names. Imports with the same name have equal types, because every one of them was resolved in the one `Resolve` of the build (§7.1.1 stage 1, package/04 §4.5.2 item 3).
4. **Structure.** The composed component `C` contains, in this order:
   - one import per outer import, with its name and instance type;
   - the bytes of every member of `U` and of `C₀`, each embedded unchanged as a nested component;
   - one instance of every member of `U`, then one instance of `C₀`. The instances are created in a topological order of the provider relation, providers first, with ties broken by the bytes of the identities, so `C₀` is instantiated last. Each instantiation passes, for every import of the instantiated component, the export of the provider's instance if the import has a provider for it, and the outer import of the same name otherwise. A member of `U` is instantiated once, and its instance serves every importer;
   - one export per export of `C₀`, aliased from the instance of `C₀` with the same name and type.

   The exports of the members of `U` are not exported. Exports of an artifact that are not in its world are hidden in the same way (package/04 §4.5.2 item 3), so the exports of `C` are exactly those of `W'` (N-D-103).
5. **Encoding.** `C` is encoded with `wasm-encoder` (D-TOOL-05). It has no custom section of its own, and the nested components keep theirs unchanged. Its bytes are a function of `C₀`, the bytes of the artifacts in `U` and the pinned `wasm-encoder` version, so §7.11.6 and §7.1.2 hold for it.
6. **No artifact used.** When `U` is empty, the build's component is `C₀` itself, byte for byte (N-D-103).
7. **Failures.** Analysis computes items 1 to 3 and reports `WORLD_IMPORT_UNDECLARED` for the outer imports, and `PROVIDED_INTERFACE_AT_BOUNDARY` for a provided interface that would cross the boundary of `C` (defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; cases in [package/04](../package/04-distribution-and-security.md) §4.5.3 item 5), before lowering starts ([impl/08](08-effect-and-capability-analysis.md) §8.7.9 items 6 and 7). Every artifact is verified, and every import and export type was compared in the one `Resolve` of the build. Composition therefore cannot fail on a build that passed analysis, and any failure, including one found by step 4 of §7.13.1, is `INTERNAL_COMPILER_ERROR` (N-D-62).

```rust
pub enum Importer { Root, Artifact(PackageId) }

pub struct CompositionPlan {
    pub used: Vec<PackageId>,                          // U, in instantiation order (item 4)
    pub links: Vec<(Importer, ImportName, PackageId)>, // imports with a provider (item 1), by importer, then name bytes
    pub outer: Vec<ImportName>,                        // outer imports (item 3), by name bytes
}
```

`composition_plan(K)` computes the plan once per build key, from `Imp` and the component types of the artifacts. [impl/08](08-effect-and-capability-analysis.md) §8.7.9 uses it for `WORLD_IMPORT_UNDECLARED`, and this section uses it to encode `C`. An `Importer::Root` ranks before every `Importer::Artifact`, and artifacts rank by the bytes of their identities.

### 7.11.10 In-build provision

A hybrid dependency used in source form provides the interfaces of its `export world` inside the root's core module ([spec/13](../spec/13-components-and-wit.md) §13.5.2 and N-D-104, [package/04](../package/04-distribution-and-security.md) §4.5.3 item 4). Provision happens for every build key, of every target and every kind of artifact, core modules included. It links calls inside the core module and creates no core import, no core export and no item of the component type (N-D-107). A native backend (Stage 7) lowers the same glue.

**Plan.**

1. **Providers.** Let `S` be the active nodes, other than the root, that are hybrid packages used in source form (package/04 §4.5.1). An interface `I = ns:pkg/iface@v` is **provided within the build** by `n ∈ S` if all of the following hold:
   - `n` has the identity `ns:pkg` and the selected version `v`;
   - `I` is not in the `wasi` namespace;
   - the elaborated export world `W_n` of `n` exports `I`.

   By the argument of §7.11.9 item 1, `n` is unique when it exists, and two compatibility lines of one package are two providers. Each is matched with the SAI view of its own version, which package/04 §4.5.3 item 2 checks against its source. Functions that `W_n` exports directly, outside an interface, are never provided.
2. **Calls.** Every call of a function of a provided interface, and every drop of a handle of a provided resource, is linked to the provider, whichever package of the build makes it. A call of a package's own impl ([spec/13](../spec/13-components-and-wit.md) N-D-29) is not provision; in particular, the provider calls its own interfaces through its impls.
3. **Resources.** The **provided resources** are the resources defined in provided interfaces. Their **type indices** are `k = 1, 2, …`, in increasing byte order of `ns:pkg/iface@v#r`. They are the `kind` values of the handle table ([10](10-runtime.md) §10.3.3).
4. **Independence.** The plan depends only on the active graph, the forms and the worlds `W_n`, never on reachability. [impl/08](08-effect-and-capability-analysis.md) can therefore use it to compute reachability without a cycle.

```rust
pub struct ProvisionPlan {
    pub provided: Vec<(ImportName, PackageId)>,          // provided interfaces (item 1) and their providers, by name bytes
    pub resources: Vec<(ImportName, Symbol, PackageId)>, // provided resources (item 3): interface, WIT name, provider; type index = position + 1
}
```

`provision_plan(K)` computes the plan once per build key, for every key. [impl/08](08-effect-and-capability-analysis.md) §8.7.3 uses it for reachability and §8.7.9 for the imports of the component, and this section uses it to generate glue.

**Glue.** Let `f` be a reachable function of a provided interface `I` (impl/08 §8.7.3), with WIT function type `ft` and provider `n`. Let `M` and `F` be its core module and field names as an import (§7.10.1), and `E` its core export name (§7.10.2), which `n` would export if it were built as the root. Component lowering generates the following synthetic bodies, in addition to the import glue that the callers of `f` already use (N-D-107):

| Body | Symbol | Core type | Content |
|---|---|---|---|
| caller's import glue | `glue:M#F` | the SAI view signature | §7.7.2, except that step 2 calls the adapter instead of a core import |
| adapter | `glue:[adapter]M#F` | `core_signature(ft, Import)` | the algorithm below |
| provider's export glue | `glue:[provide]E` | `core_signature(ft, Export)` | §7.8.2 for `n`'s implementation of `f`, with the differences below |
| provider's post-return | `glue:[provide]cabi_post_E` | `(param ptr)` | §7.5.5, if `f` has a post-return function |

For every provided resource `r` of `I` whose drop glue is reached, or that a reachable function of `I` passes or returns, it also generates:

| Body | Symbol | Core type | Content |
|---|---|---|---|
| provider's destructor | `glue:[provide]ns:pkg/iface@v#[dtor]r` | `(param ptr)` | `[dtor]r` of §7.6.3 |
| drop adapter | `glue:[adapter]ns:pkg/iface@v#[resource-drop]r` | `(param i32)` | `p := __sai_handle_drop(k, h)`, then the provider's destructor `[dtor]r(p)` (§7.6.5) |

The provider's export glue differs from §7.8.2 as follows:

- It is called only by the adapter. It is neither a core export nor a root.
- The intrinsics `[resource-new]r`, `[resource-rep]r` and `[resource-drop]r` of a provided resource are the replacements of §7.6.5, not core imports from `[export]…`.
- Step 3 calls no `[resource-drop]` for a `borrow R` parameter, whatever the resource: the adapter has passed the caller's handle itself, or the rep, and no borrowed handle exists (§7.6.5).
- The boundary checks of step 2 run as for an export of the root ([spec/09 §9.7.3](../spec/09-contracts-and-refinements.md)), because the provider is entered across a component boundary.

**Adapter.** The adapter does what the Component Model does for a call between two instances, here two instances that share one memory and one allocator. It performs the lift from the caller and the lower into the provider, and the same for the results. Both `realloc` options are the allocation of `cabi_realloc` ([10](10-runtime.md) §10.3.1), performed through the allocation primitive of §7.6.3. It does not validate characters, discriminants or UTF-8, because both sides are glue of this module (N-D-48). Handles are checked by the handle table (§7.6.5).

```text
adapter_f(vals: the parameters of core_signature(ft, Import)):
    // 1. Arguments, in parameter order, and within a parameter in WIT field order (§7.4).
    read the arguments from vals, or from the caller's spilled tuple P
    for every string or list<t> in them, at any depth:
        len = 0:  pass the pointer align(t) and allocate nothing
        len > 0:  q := allocate(len · size(t), align(t)); copy the elements into q,
                  converting each nested string, list and handle by the same rules; pass q
    for every handle in them: the rule of §7.6.5 (lend, move, or unchanged)
    if core_signature(ft, Export) spills the parameters:
        Q := allocate(size(tuple<p1, …, pn>), align(tuple<…>)); store the converted arguments in Q
        args := [Q]                                   // freed by step 1 of the provider's export glue
    else:
        args := the converted flat arguments

    // 2. Call.
    out := Call glue:[provide]E(args)

    // 3. Results.
    read the result from out, or from __cabi_ret_area when it is spilled
    copy every string and list in it, at any depth, into fresh blocks by the rules of step 1
    write the copied result into the caller's return area A (the last value of vals) when
        core_signature(ft, Import) spills it, and into the flat results otherwise

    // 4. Post-return, when f has a post-return function (§7.5.5).
    Call glue:[provide]cabi_post_E(__cabi_ret_area)

    // 5. End the loans of step 1, in reverse order.
    for every handle h lent in step 1: __sai_handle_unlend(h)
    return the flat results
```

- The order of the steps is that of the Component Model: arguments are lifted and lowered before the call, results are copied before post-return, and loans end when the call has completed. A trap therefore occurs at the same point of the call as it would in artifact form.
- The blocks that step 1 allocates belong to the provider, which frees them as an export frees the buffers that the host allocated (§7.8.2 step 3). The blocks that step 3 allocates belong to the caller, whose import glue lifts them without copying (§7.5.2). The own set of the call is therefore the same as in artifact form ([spec/08 §8.14.3](../spec/08-effects-and-capabilities.md); impl/08 §8.7.4).
- On a wasm64 target, `ptr` is `i64`, and the pointer and the length of a `string` or `list` are `PS` bytes each, so the pair has size 16 and alignment 8. This layout is private to the module and needs no support from `wit-component` or the host (contrast §7.11.7; N-D-109).

**Further rules.**

- **Order.** Provision glue is generated in increasing byte order of its symbols. The adapters replace core imports one for one, so the core imports of a module with provision are those of §7.10.3 without the provided ones.
- **Async.** An item of a provided interface whose signature is `async` or contains `future<T>` or `stream<T>` is rejected where a SAI program uses it, exactly as for an imported interface ([spec/13](../spec/13-components-and-wit.md) §13.9.2). Component lowering therefore never sees one. Fused async calls belong to Stage 5.
- **Traps and panics.** A trap or panic in the provider, in the adapter or in the handle table traps the module. This ends the instance, and no destructor runs, as in artifact form, where a trap in a composed artifact ends the composed component (§7.12).
- **Reentrance.** A provider never calls back into a caller, because the active graph is acyclic and WIT has no function values. Provided calls therefore nest strictly, and the areas of §7.5.3 are shared safely.
- **Failures.** `HYBRID_WIT_MISMATCH` (package/04 §4.5.3 item 2) guarantees that the caller's `core_signature(ft, Import)` and the provider's `core_signature(ft, Export)` come from the same `ft`. Any mismatch found while generating provision glue is `INTERNAL_COMPILER_ERROR` (N-D-62).

---

## 7.12 Traps at the boundary

| Condition | Detected by | Outcome |
|---|---|---|
| a `char` that is not a Unicode scalar value | the runtime, when it lifts a value lowered by SAI | trap |
| a discriminant `≥ n` for a variant or enum of `n` cases | the runtime, when lifting | trap |
| a `string` that is not valid UTF-8 | the runtime, when lifting | trap |
| a pointer and length outside linear memory, or a misaligned pointer | the runtime, when lifting or when storing through a pointer returned by `cabi_realloc` | trap |
| a handle index that is not a live handle of the expected resource type | the runtime | trap |
| dropping an owned handle while it is lent | the runtime | trap |
| a borrowed handle not released when an export returns | the runtime | trap; cannot happen with the glue of §7.8.2 |
| a call into the instance while one of its exports is running or before its post-return function has run | the runtime | trap |
| `cabi_realloc` called with an alignment not in {1, 2, 4, 8} | `cabi_realloc` | trap |
| an allocation by `cabi_realloc` or by glue that cannot be satisfied | the allocator ([10-runtime.md](10-runtime.md) §10.2.6) | trap |
| a parameter invariant or `requires` clause violated at an export | the glue's boundary checks ([09](../spec/09-contracts-and-refinements.md)) | panic, then trap |
| a panic in the implementation of an export | SAI code | trap (D-ERR-01) |
| a handle of a provided resource that is not live or of another resource type, an owned one moved or dropped while it is lent, or a full handle table | the handle table ([10-runtime.md](10-runtime.md) §10.3.3), for a call provided within the build (§7.6.5) | trap |
| a panic or trap in the provider of a call provided within the build | SAI code | trap, as in a composed artifact (§7.11.10) |

- SAI never lowers an invalid value unless `unsafe` code has created one, which is undefined behavior ([12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md)); the runtime's trap is then the observable result. A call provided within the build has no runtime that lifts its values, so such a value is not caught there (§7.11.10).
- Values that the runtime lowers into SAI are valid by the Component Model's guarantees; glue does not re-validate them (N-D-48).
- Every trap ends the instance; no destructor runs ([spec/13 §13.10.4](../spec/13-components-and-wit.md)).

---

## 7.13 Validation and testing

### 7.13.1 Validation in every build

Every component build performs, in this order:

1. validation of the core module by `wasmparser` with exactly the features of [06 §6.2](06-wasm-lowering-and-layout.md);
2. validation of the core module against the synthetic world by `wit-component` (the encoder's validation step: every import has a known name and type, every world export has a core export of the right type, `cabi_realloc`, `memory` and post-return functions have the right types);
3. validation of the resulting component by `wasmparser` with the component model enabled;
4. if the build composes artifact dependencies (§7.11.9), validation of the composed component by `wasmparser` with the component model enabled, and a check that its imports are the outer imports of the plan and its exports are those of `C₀`.

Every user error is reported before component lowering starts, so a failure in any of these steps is a compiler bug. It is reported as `INTERNAL_COMPILER_ERROR` ([11-diagnostics.md](11-diagnostics.md) §11.4.6) with the validator's message, and no artifact is written (N-D-62).

### 7.13.2 Test suites

The reference implementation MUST include the following tests, which are part of the compiler test strategy of [15-conformance-and-testing.md](15-conformance-and-testing.md):

- **Signature agreement.** For every function of every WIT package in the test corpus (including all of WASI 0.2 at the supported version), the core signature of §7.3.3 equals the signature that `wit-parser` computes for the guest import and guest export variants, and `size`, `align` and field offsets equal those of `wit-parser`'s size-and-alignment computation.
- **Golden WIT.** For each fixture package, the generated WIT ([spec/13 §13.11](../spec/13-components-and-wit.md)) and the world printed from the built component by the WIT decoder (`wasm-tools component wit` equivalent) are compared with checked-in expected text.
- **Round trip through a host.** For each type shape (every scalar, `string`, nested `list`, records with reordered fields, every `option`/`result` form, variants whose cases flatten to different types, enums with 2, 256 and 257 cases, flags with 1, 8, 9, 16, 17 and 32 flags, imported and exported resources, spilled parameters and results), a component exports `echo: func(x: T) -> T` and imports `host-echo: func(x: T) -> T`. A `wasmtime` harness calls `echo` with values from a seeded deterministic generator; the SAI implementation passes each value through `host-echo` and returns it. The harness checks equality, that post-return ran, and that the allocator's count of live bytes returns to its value before the call.
- **Resource lifecycle.** Destructors run exactly once per value; a value consumed by an export taking `own<r>` is not dropped by the destructor; borrowed handles of imported resources are released before the export returns; a client that drops a resource while it is lent traps.
- **Pruning and allocation.** The component's imports are exactly `Imp`; a component that meets none of the conditions of §7.5.1 exports no `cabi_realloc`; a component whose boundary does not allocate ([spec/08 §8.14.3](../spec/08-effects-and-capabilities.md)) and whose reachable code does not use `alloc` contains no allocator (D-WASM-05).
- **Determinism.** Building the same package twice, in different directories and with different thread counts, produces byte-identical core modules and components.
- **Traps.** Fixtures with `unsafe` code that produce an invalid `char`, an invalid discriminant and invalid UTF-8 in export results trap at the boundary.
- **Commands.** A command returning success and failure exit codes is run by `wasmtime`, and the host's exit status is checked to be zero and non-zero respectively.
- **Composition.** Fixtures with artifact dependencies cover each item of §7.11.9:
  - a root that calls an artifact's interface, where the composed component has no import of that interface and `wasmtime` runs the call through the artifact;
  - two artifacts that share a third, which is instantiated once;
  - an artifact import that has no provider, which becomes an outer import, and is `WORLD_IMPORT_UNDECLARED` when `W'` lacks it;
  - an artifact that no import uses, which is not embedded but whose imports are still checked against the policies;
  - an artifact with exports outside its world, which are not exported;
  - a build without artifact dependencies, which is byte-identical to `C₀`;
  - repeated builds, which are byte-identical.
- **Provision.** Fixtures with a hybrid dependency in source form cover §7.11.10 and §7.6.5:
  - the round-trip shapes above, called through a provided interface on `wasm32-core`, `wasm64-core` and `wasm32-wasi`, with the same results, the same post-return behaviour and the same balance of live bytes as the same hybrid in artifact form on `wasm32-wasi`;
  - provided resources: destructors run exactly once, a value consumed through `own<r>` is not dropped, and forged handles (made with `unsafe` code) that are freed, of another resource type, or moved or dropped while lent trap;
  - two compatibility lines of one hybrid, each provider matched with its own version;
  - a component of a core target whose only interface calls are provided, which has no imports, and a provider whose own import is `WORLD_IMPORT_UNDECLARED` or, on a core target, `TARGET_CAPABILITY_UNAVAILABLE`;
  - each case of `PROVIDED_INTERFACE_AT_BOUNDARY`;
  - a provided function that no reachable code calls, whose glue and impl are absent after DCE.

---

## 7.14 Decisions introduced by this chapter

**N-D-47 Zero fill of joined positions.** When a flattened variant case uses fewer core values than the joined signature, or a narrower type than the joined position, glue writes zero into the unused positions and bits (§7.3.2). *Rationale:* the Canonical ABI ignores them, but writing a fixed value makes the generated code independent of stale register contents and therefore deterministic (P8).

**N-D-48 Values lowered by the runtime are trusted.** Glue does not re-validate values that the runtime has lowered into the component (characters, discriminants, UTF-8, handles, bounds); it relies on the Component Model's guarantees. *Rationale:* the runtime already validates every value it lifts from the other side, so a second check would only add code (P7) and could never fail (P9).

**N-D-49 `cabi_realloc` is exported only when needed.** It is exported exactly under the conditions of §7.5.1, and canonical functions carry a `realloc` option only then. *Rationale:* a component that never receives runtime-allocated memory must not contain an allocator it does not use (D-WASM-05, P7); the conditions depend only on WIT types, so the result is predictable (P8).

**N-D-50 Component runtime state.** Component lowering adds at most three runtime-state items, `__cabi_ret_area`, `__cabi_pending` and `__cabi_consumed_rep`, each only if used, zero-initialized, written only by glue, and placed after the items of [10-runtime.md](10-runtime.md) (§7.5.3). *Rationale:* the Component Model's non-reentrance means one area of each kind suffices (P9); static areas avoid a hidden allocation per call (P7, P8).

**N-D-51 Post-return functions.** An export has a post-return function exactly when its WIT result contains a `string` or a `list`. The function frees converted buffers with `free_converted` and releases the pending SAI result held in `__cabi_pending` with `release` (§7.5.5). *Rationale:* these are the only results whose memory must survive the export's return (P9); keeping the SAI value intact until post-return avoids copying or shrinking buffers, so no hidden allocation occurs (P7, P8).

**N-D-52 Internal layout and Canonical ABI are kept separate.** SAI's internal layout is never constrained to match the Canonical ABI, except that flags types use `U8`, `U16` or `U32` with bit `i` for flag `i`; glue converts at the boundary, and lists share buffers only when `cabi_compatible` holds for the actual layouts (§7.9). *Rationale:* D-WASM-03 makes internal layout compiler-private; the public ABI is the portable Canonical ABI (P4); the flags exception costs nothing and makes flags zero-copy (P7).

**N-D-53 Exported resource reps are plain boxes.** The rep of an exported resource is the address of a `Box<C>` allocation holding the value; a value moved out by an export that consumes `own<r>` is marked by the single slot `__cabi_consumed_rep`, which the destructor checks and resets (§7.6.3). *Rationale:* D-WASM-03 and [06 §6.4.14](06-wasm-lowering-and-layout.md) fix the rep as `Box<R>` usable directly as `borrow R`, which forbids a state header in the allocation; the Component Model runs the destructor synchronously inside `resource.drop`, so one slot is exact (P9).

**N-D-54 The release walk runs no `Drop` impl.** When a SAI value's content has been transferred across the boundary, glue frees the buffers it owns without running `Drop` impls, and does nothing for resource values (§7.5.4). *Rationale:* the value's content now belongs to the other side, so running user code on it would act on transferred data; skipping `Drop` is unobservable because `Drop` is pure and deallocation is not an effect (D-OWN-05, P9).

**N-D-55 Core names follow the `wit-component` conventions.** Core imports and exports use the module and field names of §7.10.1–§7.10.2 (the conventions that `wit-component` recognizes by default), and no other name mangling. *Rationale:* the names are consumed only by `wit-component` (D-TOOL-05), so using its default conventions needs no extra configuration and keeps the core module inspectable with standard tools (P4, P9).

**N-D-56 The `component-type:sai` custom section.** The component type is embedded as a custom section named `component-type:sai`, produced by `wit-component`'s metadata encoder with `utf8` strings and placed last (§7.11.1). *Rationale:* a fixed name and position keep the core module byte-identical across builds (P8); the `component-type:` prefix is what `wit-component` looks for.

**N-D-57 The synthetic world.** The component is described by a synthetic world in a synthetic package `sai:root` whose imports are `Imp` and whose exports are those of `W'` plus `wasi:cli/run` for commands (§7.11.2). *Rationale:* the component's imports must be exactly the pruned set of [spec/13 N-D-41](../spec/13-components-and-wit.md) (P5, P7); the synthetic names are not part of any component type, so they cannot be observed (P8).

**N-D-58 Components for `wasm64-wasi` are not implemented.** A component build for `wasm64-wasi` stops with `FEATURE_NOT_IMPLEMENTED` after the core module has been produced and validated (§7.11.7). *Rationale:* the target is experimental (D-TGT-01) and the pinned `wit-component` and `wasmtime` do not support the Canonical ABI on 64-bit memories; an explicit diagnostic is better than an unvalidated artifact (P1, P9).

**N-D-59 Panic reports never add imports.** In a component, the panic report uses `wasi:cli/stderr` only if that interface is already in `Imp`; otherwise a panic traps without a report (§7.11.8). *Rationale:* the imports of a component are its capability requirements, and they must not depend on whether code can panic (P5, P8); panics have no effect ([spec/08](../spec/08-effects-and-capabilities.md)).

**N-D-60 Sorted core imports and exports.** Core imports are ordered by (module, field) bytes and core exports by name bytes (§7.10.3–§7.10.4). *Rationale:* function indices and section contents then do not depend on traversal order, which makes the output deterministic and keeps small source changes from renumbering unrelated imports (P8).

**N-D-61 No adapter, start function or `_initialize`.** Wrapping uses no adapter module, and the core module has neither a start function nor an `_initialize` export (§7.11.3). *Rationale:* WASI preview 1 is not a target (D-TGT-01); SAI has no static initialization (D-CAP-02), so there is nothing to run before the first export (P7, P9).

**N-D-62 Validation failures are compiler bugs.** Every component build validates the core module, the module against its world, and the component; any failure is `INTERNAL_COMPILER_ERROR` and no artifact is written (§7.13.1). *Rationale:* all user errors are reported before component lowering, so an invalid output can only be a compiler bug (P2), and writing it would hide that bug (P1, P9).

**N-D-63 Normalized empty buffers.** A lifted `string` or `list` of length 0 ignores the incoming pointer and uses SAI's empty representation, and frees nothing; a lowered empty value passes SAI's own pointer (§7.4.3–§7.4.4). *Rationale:* the Canonical ABI allows any aligned pointer for an empty buffer, so adopting it could make SAI free a pointer it never allocated; one representation of the empty value is simpler and deterministic (P8, P9).

**N-D-64 Glue is synthetic MIR.** All boundary glue is generated as synthetic MIR in phase `Mono`, lowered by [06](06-wasm-lowering-and-layout.md) like other bodies and optimized by [09](09-optimization-and-size.md), but never borrow-checked and never stripped or renamed at core imports and exports (§7.1.1). *Rationale:* one backend for all code keeps layout decisions in one place (P9); glue uses raw memory by construction, and its correctness is established by the tests of §7.13.2 rather than by the borrow checker (P2).

**N-D-102 Providers of composed imports.** An import of the root's component or of an artifact is satisfied inside the composition exactly when an artifact package of the same WIT package and version exports it and, for an artifact importer, is one of its dependencies; an import in the `wasi` namespace is always left to the host, and the root's exports never satisfy an import (§7.11.9 items 1 and 2). *Rationale:* identity equals the WIT package name, and a graph has one version of it per compatibility line, which the import name carries, so the provider is unique and needs no configuration (P9); requiring the provider to be a dependency makes the instantiation order acyclic and follows the package graph that the user declared (P8), and leaving WASI to the host keeps the authority of the component visible in its imports (P5).

**N-D-103 Structure of a composed component.** The composed component imports the unsatisfied imports once each, embeds the used artifacts and the root's component unchanged, instantiates them providers first with the root's component last, and exports exactly the exports of the root's component; without a used artifact, the root's component is the output unchanged (§7.11.9 items 3 to 6). *Rationale:* embedding verified bytes unchanged keeps the checksums of package/04 meaningful (P5), exporting only the root's exports keeps `W'` the contract of the artifact ([spec/13](../spec/13-components-and-wit.md) §13.5.1, P1), and the fixed order makes the bytes deterministic (P8).

**N-D-107 Provision glue.** A call of a function of an interface provided within the build goes from the caller's import glue to an adapter `glue:[adapter]M#F`, which copies the arguments into fresh blocks, calls the provider's export glue `glue:[provide]E`, copies the results for the caller and then calls the provider's post-return; provision glue is generated for every target, is neither a core import, a core export nor a root, and validates no value other than handles (§7.11.10). *Rationale:* reusing the import and export glue of both sides gives the call exactly the semantics, the own sets and the trap points of the same call in artifact form, so the form of a hybrid package changes no observable behavior (P8, P4); copying keeps the provider's ownership of parameter buffers that its export glue frees, and skipping validation follows N-D-48, because both sides are glue of the same module (P9).

**N-D-108 In-module handles of provided resources.** A handle of a resource provided within the build is an index into the in-module handle table of [10](10-runtime.md) §10.3.3, whose `kind` is the type index of the provision plan; the provider's resource intrinsics, the drop adapter and the lending of borrowed handles use that table, which traps exactly where the Component Model's resource table traps, and the provider's export glue drops no borrowed handle (§7.6.5, §7.11.10). *Rationale:* the provider and its callers keep the handle representation of §7.6.2 and §7.6.3, so their glue needs no second form (P9); CM-identical checks keep resource types distinct and keep the traps of artifact form (P2, P8); and a table inside the module works on core targets, which have no resource table (P4).

**N-D-109 Private wasm64 layout of provision glue.** On a wasm64 target, provision glue uses `i64` pointers and `PS`-byte pointers and lengths in memory, unlike the Canonical ABI's 32-bit layout (§7.11.10). *Rationale:* provision glue never crosses the module boundary, so it needs no support from `wit-component` or a host (contrast N-D-58), and using SAI's own pointer size avoids a narrowing check on every buffer (P9).

---

## 7.15 Diagnostics

Component lowering reports no user error of its own: every condition a user can cause is diagnosed earlier ([spec/13](../spec/13-components-and-wit.md), [spec/08](../spec/08-effects-and-capabilities.md), [08](08-effect-and-capability-analysis.md)). It emits the following codes, all defined elsewhere.

| Code | Severity | Description |
|---|---|---|
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; a component build for `wasm64-wasi` (§7.11.7, N-D-58) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6; a validation failure of the core module, of the module against its world, of the component, or of a composed component (§7.13.1, N-D-62), a failure to compose artifact dependencies (§7.11.9 item 7), or a WIT signature that cannot arise from a valid world (§7.6.1). |
| `PROVIDED_INTERFACE_AT_BOUNDARY` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; reported by [08](08-effect-and-capability-analysis.md) §8.7.9 item 7 before lowering, so component lowering never sees an interface provided within the build or by composition at the boundary of the root's component (§7.11.9 item 7, §7.11.10). |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; referenced: reported by [08](08-effect-and-capability-analysis.md) before lowering when a component build reaches `extern "wasm"` (host) |
| `WORLD_IMPORT_UNDECLARED` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; referenced: reported by [08](08-effect-and-capability-analysis.md) §8.7.9 item 6 before lowering for an outer import of a composition that `W'` does not import (§7.11.9 item 7) |
