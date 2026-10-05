# 06. Wasm Lowering and Layout

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 1 (wasm32-core: scalars, functions, structs, enums, `Option` / `Result`), 2 (ownership, generics, drop glue, closures, DCE, tiny, wasm64-core), 4 (wasm32-wasi / wasm64-wasi core modules for component assembly). Threads are reserved (D-THREAD-01, not in v0.1).
**Related:** [decisions.md](../design/decisions.md) (D-WASM-01..05, D-MIR-01, D-ERR-01, D-TGT-01, D-TGT-02) · [04-mir.md](04-mir.md) · [07-component-lowering.md](07-component-lowering.md) · [09-optimization-and-size.md](09-optimization-and-size.md) · [10-runtime.md](10-runtime.md) · [11-diagnostics.md](11-diagnostics.md) · [05-types.md](../spec/05-types.md) · [07-ownership-and-borrowing.md](../spec/07-ownership-and-borrowing.md) · [10-errors-and-panics.md](../spec/10-errors-and-panics.md) · [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) · [targets-and-profiles.md](../targets/targets-and-profiles.md)

This chapter specifies how the reference compiler turns Mono-phase MIR into a single core WebAssembly module. It covers the Wasm features the compiler may use, the linear memory layout, the size, alignment and field placement of every SAI type (spec/05 owns what the types mean; this chapter owns how they are laid out), how values live in Wasm locals, the internal calling convention, the lowering of every MIR statement, rvalue and terminator, the algorithm that turns a CFG into structured control flow, whole-program linking, and custom sections and debug information. Component wrapping (Canonical ABI lift/lower, `cabi_realloc` glue, component assembly) belongs to [07-component-lowering.md](07-component-lowering.md). Runtime functions that the generated code calls (allocator, panic, arenas, start sequence) belong to [10-runtime.md](10-runtime.md). Optimizations and size attribution belong to [09-optimization-and-size.md](09-optimization-and-size.md). Everything in this chapter is compiler-private and not a stable ABI (D-WASM-03, draft §40); only the component boundary is a public ABI.

Notation: `PS` is the pointer size in bytes (4 on wasm32 targets, 8 on wasm64 targets). `ptr` is the Wasm value type of addresses (`i32` on wasm32, `i64` on wasm64), and `ptr.const`, `ptr.add`, `ptr.load` and so on stand for the `i32.*` instructions on wasm32 and the `i64.*` instructions on wasm64. Examples are written for wasm32 unless they say otherwise. `align_up(x, a)` is the smallest multiple of `a` that is `>= x`. All sizes are in bytes. Wasm text uses the folded or flat forms of the WebAssembly text format; `$`-names are symbols defined in §6.10.2.

---

## 6.1 Backend inputs and outputs

### 6.1.1 MIR features consumed

The backend consumes MIR in phase `Mono` (D-MIR-01). The data structures belong to [04-mir.md](04-mir.md). This section lists every MIR feature the backend relies on, and the properties it assumes. If impl/04 names a feature differently, the backend maps it by meaning. A Mono body that violates a property in this list is an internal compiler error (`INTERNAL_COMPILER_ERROR`, [11-diagnostics.md](11-diagnostics.md) §11.4.6).

| MIR feature | Used for | Property relied on |
|---|---|---|
| `Body.locals`, `arg_count`, `_0` | Frame and Wasm local allocation (§6.7) | Every local has a concrete, fully monomorphic type. `_1 .. _arg_count` are the parameters in order. `_0` is the return place. In a forwarding body the last parameter is the hidden caller location `_l: Ptr<(Str, U32, U32)>` of origin `CallerLocation` ([04](04-mir.md) §4.13.7), an ordinary parameter. |
| `Body.blocks` | CFG and structuring (§6.9) | `bb0` is the entry. Every block has exactly one terminator. |
| `Body.source_scopes` and statement spans | Panic sites (§6.8.12), DWARF lines (§6.12) | Every statement and terminator has a span (file, line, column). A scope with `location: Caller(_l)` takes the location of its panics from `_l` ([04](04-mir.md) §4.2.7, §6.8.12.2). |
| `Body.phase == Mono` | Precondition | No generic parameters, no unresolved trait calls, no `Await` terminators (the async transform has run, Stage 5), drop elaboration has run. |
| `Place = Local + [Projection]` | Address computation (§6.8.1) | Projections are `Field(idx)`, `Deref`, `Index(Local)`, `ConstIndex{offset, from_end}`, `Subslice{from, to}`, `Downcast(variant)`. A `Field` that follows `Downcast(v)` names a field of variant `v`. |
| `Operand = Copy \| Move \| Const` | Operand lowering (§6.8.2) | A `Move` leaves its place uninitialized until the next assignment. |
| `Constant` | Constants and static data (§6.8.14) | A constant has one of the Mono kinds of [04](04-mir.md) §4.2.3: `Scalar` (a bit pattern), `Zst`, `Str` and `ByteStr` (literal bytes), `Alloc` (a typed static allocation of [04](04-mir.md) §4.13.6, whose bytes this chapter computes), `Aggregate` (a constant aggregate whose leaves are scalars), `Location` (the address of a location record) or `Instance` (a zero-sized function instance). Mono has no `FnItem` and no `Param` constant. |
| `Rvalue` (all twelve variants of D-MIR-01) | §6.8.4 | `CheckedBinaryOp(op, a, b)` has type `(T, Bool)`. Field 0 is the wrapped result and field 1 is the overflow flag. |
| `Statement` (all six variants) | §6.8.3 | Storage statements produce no code (§6.8.3), so the backend relies on no storage property. The rule that holds is [04](04-mir.md) §4.11.1 rule 4. |
| `Terminator` (all except `Await`) | §6.8.10 | `Call` and `Drop` have no cleanup edges (no unwinding). |
| `AssertKind` | Panic lowering (§6.8.12) | `BoundsCheck` carries the index and length operands. All other kinds carry no operands: `Overflow(op)` carries only the operation (`Add`, `Sub`, `Mul`, `Div` or `Neg`), which selects its fixed text. `Contract(id)`, `Refinement(id)` and `Invariant(id)` identify the source text of the predicate. |
| Drop glue bodies | `Drop` lowering (§6.8.10.6) | For every type `T` that needs drop glue, impl/04 provides a Mono body `drop<T>(v: T) -> ()`, which consumes its argument. Glue that can panic is forwarding and has the signature `drop<T>(v: T, loc)`; impl/04 turns every `Drop` of such a type into a `Call` with a location argument, so a `Drop` terminator names glue without a location parameter ([04](04-mir.md) §4.13.7, §4.13.8). |
| `ConvertKind` | Casts (§6.8.8) | The kinds listed in §6.8.8. |
| Callees | Calls (§6.8.9) | Every callee is one of the kinds of [04](04-mir.md) §4.11.5 rule 4. The intrinsics left to the backend are `size_of`, `align_of` and `zeroed` ([04](04-mir.md) §4.13.9) and the panic intrinsics, whose last argument is the location ([04](04-mir.md) §4.13.7). |
| Checks are explicit | §6.8.6, §6.8.7 | MIR building emits `Assert` terminators for overflow, division by zero, `MIN / -1`, shift amount, negation of `MIN`, bounds, contracts, refinements and invariants. The backend never invents a semantic check. It only emits the hardware-level code those `Assert`s and `BinaryOp`s require. |

The backend does not consume: borrow regions (it ignores the `region` of `Borrow`), `FakeRead`, effects on `Call` (D-CAP-01 is checked before the backend runs; see [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md)), or `source_scopes` beyond spans and the inlining records that size attribution reads (§6.11.5).

### 6.1.2 Other inputs

| Input | Source | Content |
|---|---|---|
| Item table | impl/04 / impl/09 | The set of reachable Mono function instances (after item-level DCE, [09-optimization-and-size.md](09-optimization-and-size.md) §9.6), each with its symbol (§6.10.2), its kind (ordinary, `export fn`, `extern "wasm"` import, runtime function, drop glue, intrinsic) and its attributes (`@export_name`, `@import_name`, `@inline`, `@cold`, `@panic_hook`). |
| Static allocations | impl/04 | The typed allocations `StaticAlloc { ty, value }` of [04](04-mir.md) §4.13.6: promoted constants and location records. This chapter computes their bytes, alignment and relocations from the layout of `ty` (§6.8.14.2). |
| Target | [targets-and-profiles.md](../targets/targets-and-profiles.md) §2 | One of `wasm32-core`, `wasm32-wasi`, `wasm64-core`, `wasm64-wasi`. Fixes `PS` and the feature set. |
| Profile | [targets-and-profiles.md](../targets/targets-and-profiles.md) §5 | `dev`, `release` or `tiny`. Fixes the panic reporting mode (§6.8.12), the custom sections (§6.11), DWARF (§6.12) and the copy threshold (§6.8.13). The profile never changes the meaning of the program (C-03). |
| Opt-in features | [13-cli.md](13-cli.md) | The opt-in Wasm features of §6.2.1, `tail-call` and `simd128`. Both are off by default. |
| Runtime requirements | [10-runtime.md](10-runtime.md) | The runtime functions and runtime state the program reaches. |

### 6.1.3 Outputs

1. **One core Wasm module** (binary format, version 1). On a WASI target, impl/07 wraps it into a component. On a core target it is the final artifact (D-TGT-01).
2. **A size attribution table** (§6.11.5), consumed by `sai size` ([09-optimization-and-size.md](09-optimization-and-size.md) §9.10).
3. **A layout table** for `sai inspect layout` (D-TOOL-03, §6.4.16).
4. **Boundary metadata** for impl/07: the core function index and lowered core signature of every function that impl/07 must lift or lower, the symbol of `cabi_realloc` ([10-runtime.md](10-runtime.md) §10.3), and the memory export name (§6.3.7).

### 6.1.4 Backend pipeline

The backend runs the following steps in this order. Each step is deterministic: its output depends only on its inputs, never on hash iteration order, thread scheduling, time or the environment (draft §72; [09-optimization-and-size.md](09-optimization-and-size.md) §9.9).

1. **Layout.** Compute `Layout` (§6.4) for every type that appears in a reachable body or signature. Layouts are memoized per canonical type.
2. **Signature lowering.** Lower every reachable function signature (§6.6.2).
3. **Frame analysis.** For every body, classify locals as register-class or memory-class (§6.5.1) and assign frame slots (§6.7).
4. **Body lowering.** Lower statements and terminators into an unstructured *Wasm CFG*: one straight-line instruction sequence per MIR block, plus a structured terminator descriptor (§6.8).
5. **Structuring.** Turn the Wasm CFG into structured Wasm control flow (§6.9).
6. **Linking.** Assign indices, lay out static data, build the table, and emit sections (§6.10, §6.3).
7. **Custom sections and side outputs.** Emit names, producers, metadata and DWARF per profile (§6.11, §6.12), and the attribution and layout tables.

---

## 6.2 Wasm feature baseline

### 6.2.1 Feature table

The baseline is WebAssembly 2.0 (D-WASM-01). The table lists every relevant proposal and how the reference backend uses it. "Used" means the backend may emit it whenever lowering calls for it. "Never" means the backend never emits it, and a module that contains it is not a valid output of this backend.

| Feature | wasm32-core | wasm32-wasi | wasm64-core | wasm64-wasi | Notes |
|---|---|---|---|---|---|
| Mutable globals | Used | Used | Used | Used | `__sp` and runtime globals (§6.3.6). Mutable globals are never imported or exported. |
| Sign-extension operators | Used | Used | Used | Used | Canonical form of narrow integers (§6.5.2). |
| Non-trapping float-to-int | Used | Used | Used | Used | `trunc_sat` for every float-to-int conversion (§6.8.8). The trapping `trunc` forms are never emitted. |
| Multi-value | Used | Used | Used | Used | Function results only (up to 4, D-WASM-04). Block and loop types are always `[] -> []`. |
| Bulk memory | Used | Used | Used | Used | `memory.copy` and `memory.fill` only. `memory.init`, `data.drop`, `table.init`, `elem.drop` and `table.copy` are never emitted, so there is no data count section. |
| Reference types | Used | Used | Used | Used | Only to the extent of a single `funcref` table and `call_indirect` on table 0. `externref`, `ref.func`, `ref.null`, `ref.is_null`, `table.get`, `table.set`, `table.grow`, `table.size`, `table.fill`, `select` with type annotations and multiple tables are never emitted. |
| memory64 | Never | Never | Used (required) | Used (required) | Memory 0 is a 64-bit memory. The table stays 32-bit (`table64` is never used), so function pointers stay `i32` (§6.4.9). |
| Tail calls | Opt-in | Opt-in | Opt-in | Opt-in | `return_call` / `return_call_indirect` under the rule of §6.8.10.5 (N-F-1). |
| Fixed-width SIMD (`simd128`) | Opt-in | Opt-in | Opt-in | Opt-in | Enabling it has no effect on code generation in v0.1. It only adds `simd128` to `target_features` (N-F-1). |
| Relaxed SIMD | Never | Never | Never | Never | |
| Threads (atomics, shared memory) | Never | Never | Never | Never | D-THREAD-01: not implemented in v0.1. |
| Exception handling (any version) | Never | Never | Never | Never | D-WASM-01, D-ERR-01 (no unwinding). |
| GC, typed function references | Never | Never | Never | Never | D-WASM-01. |
| Multi-memory | Never | Never | Never | Never | One memory, index 0. |
| Extended constant expressions | Never | Never | Never | Never | All constant expressions are a single `i32.const` / `i64.const`. |
| Start section | Never | Never | Never | Never | Initialization is described in [10-runtime.md](10-runtime.md) §10.7. |
| Custom page sizes, wide arithmetic, stringref, branch hinting | Never | Never | Never | Never | |

The feature set depends only on the target and the opt-in features, never on the profile (N-F-1). A program whose MIR would require a "Never" feature cannot exist in v0.1, because no SAI construct lowers to one.

### 6.2.2 Opt-in features

An opt-in feature is enabled per build through the CLI ([13-cli.md](13-cli.md) owns the syntax). Enabling one MUST NOT change the meaning of any program (P8). Its only effects are:

- `tail-call`: some calls lower to `return_call` (§6.8.10.5). This removes stack growth for those calls. A program that would overflow the shadow stack without tail calls still overflows it, because tail calls only apply to functions that have no shadow frame.
- `simd128`: none in v0.1.

### 6.2.3 Instruction subset

The backend emits only instructions from this list, plus the feature-gated instructions of §6.2.1.

- Control: `unreachable`, `nop`, `block`, `loop`, `if`, `else`, `end`, `br`, `br_if`, `br_table`, `return`, `call`, `call_indirect`.
- Parametric: `drop`, `select` (untyped form, numeric operands only).
- Variable: `local.get`, `local.set`, `local.tee`, `global.get`, `global.set`.
- Memory: every `load` / `store` form for `i32`, `i64`, `f32` and `f64`, including the narrow forms; `memory.size`, `memory.grow` (runtime functions only, §6.3.8); `memory.copy`, `memory.fill`.
- Numeric: every MVP numeric instruction except the trapping float-to-int `trunc` forms; the sign-extension operators; the `trunc_sat` forms.

Memory access instructions always carry the natural alignment of the accessed scalar as their alignment hint, and a static `offset` immediate when the address is a base plus a constant (§6.8.1.3).

---

## 6.3 Linear memory layout

### 6.3.1 Address width

| Target | Memory type | `PS` | `ptr` | `USize` / `ISize` | Page size |
|---|---|---|---|---|---|
| wasm32-core, wasm32-wasi | 32-bit memory | 4 | `i32` | 4 bytes | 65536 |
| wasm64-core, wasm64-wasi | 64-bit memory (memory64) | 8 | `i64` | 8 bytes | 65536 |

### 6.3.2 Regions

When a module has a memory, memory 0 is laid out as follows (D-WASM-02). Addresses grow to the right.

```
0          8                 8+S              DATA_END    BSS_END  HEAP_BASE                 memory.size × 65536
| null     | shadow stack    | static data    | runtime   | pad    | heap (allocator,         |
| guard 8B | grows downward  | (constant)     | state     |        | grows with memory.grow)  |
```

| Region | Range | Content | Writable by generated code |
|---|---|---|---|
| Null guard | `[0, 8)` | Never holds an object. Its bytes are zero and are never read or written by code that the compiler generates. | No |
| Shadow stack | `[__stack_low, __stack_high)` = `[8, 8 + S)` | Frames of memory-class locals (§6.7). The stack grows downward from `__stack_high`. | Yes |
| Static data | `[__data_start, __data_end)` | Literals, promoted constants, panic site descriptors, constant tables (§6.8.14). Initialized by active data segments. | No. SAI has no mutable static state (D-CAP-02). |
| Runtime state | `[__bss_start, __bss_end)` | Zero-initialized state of runtime components that cannot live in globals, such as the allocator's free-list heads ([10-runtime.md](10-runtime.md) §10.2). | Runtime functions only |
| Heap | `[__heap_base, memory.size × 65536)` | Allocator-managed memory. | Yes |

`S` is the shadow stack size (§6.3.3). The ranges are always in this order. No region overlaps another.

### 6.3.3 Shadow stack size

`S = 1048576` (1 MiB) on every target and in every profile, when the module has a shadow stack (N-F-2). `S` is a multiple of 16. v0.1 provides no way to change it. A future setting would belong to [01-package-contract.md](../package/01-package-contract.md) and would have to keep the size identical across profiles.

The stack grows downward. The initial value of `__sp` is `__stack_high = 8 + S`. A function that needs a frame of `F` bytes (a multiple of 8, §6.7.2) traps through the stack overflow path when `__sp - F < 8`. The check is present in every profile (D-WASM-02, §6.6.6). Because the stack sits below static data, an overflow can never corrupt data or the heap. It reaches the guard and traps.

### 6.3.4 Modules without a shadow stack or without memory

The backend decides the following two facts after all bodies are lowered, and only then.

- `needs_stack`: at least one emitted function has a frame size `F > 0` (§6.7).
- `needs_memory`: `needs_stack`, or at least one emitted function contains a load, store, `memory.copy`, `memory.fill`, `memory.size` or `memory.grow`, or there is static data or runtime state, or the module's boundary needs a memory. A WASI target's boundary always needs a memory, because the Canonical ABI requires one. A core target's boundary needs one when some `extern "wasm"` import or `@panic_hook` passes a pointer ([10-runtime.md](10-runtime.md) §10.5.4).

Then (N-F-3):

| `needs_memory` | `needs_stack` | Layout |
|---|---|---|
| false | false | No memory section, no memory export, no `__sp` global, no data section (D-WASM-02). |
| true | false | `S = 0`. There is no `__sp` global and no stack region. `__data_start = 8`. The null guard is kept. |
| true | true | The full layout of §6.3.2 with `S = 1 MiB`. |
| false | true | Impossible: `needs_stack` implies `needs_memory`. |

The example of draft §2.3, `export fn add(a: I32, b: I32) -> I32 { a + b }` on wasm32-core, has neither: `add` has no frame, and the overflow check traps directly with `unreachable` because a core target without a `@panic_hook` has no reporting channel (§6.8.12). The module therefore has a type section, a function section, an export section and a code section, plus whatever custom sections the profile emits (§6.11).

### 6.3.5 Static data placement

Static data items are collected while bodies are lowered (§6.8.14). Each item is `DataItem { bytes: [u8], align: 1 | 2 | 4 | 8, relocations: [(offset, target)], owner: SymbolId }`, where a relocation stores the final address of another data item at `offset` in `PS` bytes. No relocation names a function: a constant contains no function value ([04](04-mir.md) §4.13.6, [spec/06](../spec/06-expressions-and-statements.md) §6.27), so static data holds no table index.

Algorithm `layout_static_data`:

```
input: items in collection order (§6.8.14.3), already deduplicated
cursor := __data_start
for a in [8, 4, 2, 1]:                       // decreasing alignment
    for item in items where item.align == a, in collection order:
        item.addr := align_up(cursor, a)
        cursor := item.addr + len(item.bytes)
__data_end := cursor
apply relocations: write each target address little-endian into the item bytes
```

Data segments are produced from the byte image of `[__data_start, __data_end)`:

```
image := bytes of [__data_start, __data_end) with gaps filled by 0
segments := []
scan the image; a run of zero bytes of length >= 16 splits segments and is omitted
leading and trailing zero runs of any length are omitted
each remaining run becomes an active segment for memory 0 with offset (ptr.const run_start)
```

Segments are emitted in increasing address order. The threshold of 16 bytes is fixed (N-F-3).

### 6.3.6 Runtime state, heap start and linker symbols

Runtime state items come from [10-runtime.md](10-runtime.md). Each has a size and an alignment (at most 8). They are placed after static data in the order of the runtime state table of [10-runtime.md](10-runtime.md) §10.1.3, each aligned to its alignment. They are zero-initialized because Wasm memory starts zeroed, so no data segment covers them.

The backend computes the following symbols. They are compile-time constants that are substituted into code and data. They are **not** exported and do not appear in the module, except through the code that uses them (N-F-3).

| Symbol | Value |
|---|---|
| `__null_guard_end` | `8` |
| `__stack_low` | `8` (present only if `needs_stack`) |
| `__stack_high` | `8 + S` (initial value of `__sp`) |
| `__data_start` | `8 + S`, or `8` if there is no stack |
| `__data_end` | end of the last static data item (§6.3.5) |
| `__bss_start` | `align_up(__data_end, 8)` |
| `__bss_end` | end of the last runtime state item |
| `__heap_base` | `align_up(__bss_end, 16)` |
| `__initial_pages` | `max(1, ceil(__heap_base / 65536))` |

The globals of the module are, in index order: `__sp` (index 0, mutable `ptr`, initialized to `__stack_high`) if `needs_stack`, then the runtime globals of [10-runtime.md](10-runtime.md) §10.1.3 that the program reaches, in symbol order (§6.10.3).

### 6.3.7 Memory section and memory export

If `needs_memory`, the module defines exactly one memory with minimum `__initial_pages` and no maximum (N-F-4). On wasm64 targets it is a 64-bit memory. The memory is never imported and never shared.

The memory is exported under the name `memory` whenever it exists, on every target (N-F-4). A WASI target needs this for the Canonical ABI (impl/07). A core target exports it so that a host can read the bytes passed by `@panic_hook` or by `extern "wasm"` imports that take pointers.

### 6.3.8 Memory growth

- Only two runtime functions execute `memory.grow` and `memory.size`: the allocator's `carve` ([10-runtime.md](10-runtime.md) §10.2.6) and the panic reporter's `report_buffer`, which places a report of more than 1024 bytes above `heap_top` ([10-runtime.md](10-runtime.md) §10.5.5). Generated code for SAI bodies never does.
- The memory never shrinks.
- A failed `memory.grow` (result `-1`) in the allocator is an out-of-memory condition ([10-runtime.md](10-runtime.md) §10.2.6). In the panic reporter it is a trap without a report ([10-runtime.md](10-runtime.md) §10.5.5).

### 6.3.9 Example address map

A wasm32-wasi program, release profile, with 300 bytes of static data and 72 bytes of runtime state has `__stack_low = 0x0000_0008`, `__stack_high = __data_start = 0x0010_0008` (the initial `__sp`), `__data_end = 0x0010_0134`, `__bss_start = 0x0010_0138`, `__bss_end = __heap_base = 0x0010_0180`, and `__initial_pages = 17` (`0x0011_0000` bytes).

---

## 6.4 Type layout

This section defines the layout of every SAI type: its size, its alignment, where its fields sit, how enum variants are encoded, which bit patterns are niches, and its scalar leaves. The meaning of the types belongs to [05-types.md](../spec/05-types.md). The layout is compiler-private and may change between compiler versions (D-WASM-03). Layouts are computed for Mono types only. Generic types have no layout until they are instantiated.

### 6.4.1 Data structures

```
Layout {
    size:      u64             // always a multiple of align
    align:     u64             // 1, 2, 4 or 8
    shape:     Shape
    leaves:    [Leaf]          // scalar leaves in increasing offset order (§6.4.13)
    niche:     Option<Niche>   // the largest niche (§6.4.11); None if none is available
    inhabited: bool            // false iff no value of the type can exist
}

Shape = Scalar(Scalar)
      | Struct { offsets: [u64], order: [u32] }   // offsets indexed by declaration index;
                                                  // order = declaration indices in memory order
      | Array  { elem: Layout, count: u64, stride: u64 }
      | Enum   { variants: [VariantLayout], encoding: Encoding }

VariantLayout { discr: i128, present: bool, payload: Layout /* Struct shape */, payload_offset: u64 }

Encoding = NoTag  { only: Option<u32> }                       // zero or one present variant
         | Tagged { tag: Scalar }                             // tag at offset 0
         | Niche  { dataful: u32, offset: u64, scalar: Scalar,
                    start: u128, others: [u32] }              // others[k] is encoded as start + k

Scalar {
    width:  1 | 2 | 4 | 8      // bytes in memory
    class:  Int { signed: bool } | Float | Ptr | FnIdx | Handle
    valid:  (start: u128, end: u128)   // inclusive range of valid bit patterns, wrapping modulo 2^(8·width)
}

Leaf  { offset: u64, scalar: Scalar }
Niche { offset: u64, scalar: Scalar }  // a scalar with available(scalar) > 0
```

For a scalar `s` with `b = 8 · s.width`:

```
available(s) = (s.valid.start - s.valid.end - 1) mod 2^b
```

A scalar whose valid range covers every bit pattern (`start = (end + 1) mod 2^b`) has `available = 0`. Float scalars always have a full valid range.

`MAX_OBJECT` is `2^31 - 1` on wasm32 targets and `2^63 - 1` on wasm64 targets (the largest `ISize`). Every size computation in this section is checked against it. A reachable Mono type whose size would exceed it is the error `TYPE_TOO_LARGE`, reported at the use that instantiated the type. The check depends only on types, so it does not depend on the profile or on optimization.

### 6.4.2 Primitive types

| Type | Size | Align | Class | Valid range | Niches (`available`) |
|---|---|---|---|---|---|
| `I8` | 1 | 1 | Int signed | full | 0 |
| `U8`, `Byte` | 1 | 1 | Int unsigned | full | 0 |
| `I16` / `U16` | 2 | 2 | Int | full | 0 |
| `I32` / `U32` | 4 | 4 | Int | full | 0 |
| `I64` / `U64`, `Int` / `UInt` | 8 | 8 | Int | full | 0 |
| `ISize` / `USize` | `PS` | `PS` | Int | full | 0 |
| `F32` | 4 | 4 | Float | full | 0 |
| `F64` | 8 | 8 | Float | full | 0 |
| `Bool` | 1 | 1 | Int unsigned | `0..=1` | 254 (values `2..=255`) |
| `Char` | 4 | 4 | Int unsigned | `0..=0x10FFFF` | `2^32 - 0x110000` (values `0x110000..=0xFFFFFFFF`) |
| `()` | 0 | 1 | — (empty tuple) | — | — |
| `Never` | 0 | 1 | — (uninhabited) | — | — |

`Int` and `UInt` are `I64` and `U64` (D-TYPE-01). `Byte` has the layout of `U8` (D-TYPE-01).

The surrogate range `0xD800..=0xDFFF` is not a valid `Char` (D-WASM-03 lists it as a niche), but the niche algorithm uses only the single wrap-around range `0x110000..=0xFFFFFFFF`, because a `Scalar` has one contiguous valid range (N-F-5). No layout ever assigns a surrogate value to an enum variant.

### 6.4.3 Pointer types

| Type | Size | Align | Class | Valid range |
|---|---|---|---|---|
| `borrow T`, `borrow mut T` | `PS` | `PS` | Ptr | `1 ..= 2^(8·PS) - 1` |
| `Ptr<T>`, `PtrMut<T>` | `PS` | `PS` | Ptr | `1 ..= 2^(8·PS) - 1` |
| `Box<T>` | `PS` | `PS` | Ptr | `1 ..= 2^(8·PS) - 1` |

The value is the linear-memory address of the pointee. The value `0` is the only niche of every pointer type (D-WASM-02). Values `1..=7` are valid bit patterns but never point to an object of nonzero size, because addresses `0..=7` hold no object.

The address of a zero-sized value is the constant `align(T)` (1, 2, 4 or 8), not the address of any storage (N-F-6). This applies to every `Borrow`, `RawPtr` and `Box` of a ZST, whatever the place is. Such a pointer is never dereferenced by a load or store, because reading or writing a ZST emits no memory access (§6.8.2). The allocator returns `align(T)` for an allocation of size 0 without allocating ([10-runtime.md](10-runtime.md) §10.2.3).

`Ptr<T>` and `PtrMut<T>` follow D-WASM-02: `0` is a niche, so a raw pointer is never null. A nullable raw pointer is written `Option<Ptr<T>>`, which has the same size as `Ptr<T>`. Producing a raw pointer with value 0 is undefined behavior; [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) owns that rule.

### 6.4.4 Views

`Str`, `Slice<T>` and `SliceMut<T>` have the layout of the struct `(ptr, len)` in this order (D-WASM-03):

| Field | Offset | Scalar |
|---|---|---|
| `ptr` | 0 | Ptr, valid `1 ..= 2^(8·PS) - 1` |
| `len` | `PS` | Int unsigned, full (`USize`) |

Size `2·PS`, align `PS`, 2 leaves, niche: the `ptr` leaf at offset 0. `Str` views UTF-8 bytes, so its element stride is 1. A view's `ptr` is never 0. An empty view that does not derive from a non-empty one has `ptr = align(T)` (1 for `Str`). Byte-string literals are `Slice<Byte>` (D-LEX-05).

### 6.4.5 Owning library types

`Box<T>`, `String`, `Vec<T>` and `Bytes` (`Vec<Byte>`) are alloc lang items (D-TYPE-02). Their layout is fixed here, whatever their source definition in [16-core-library.md](../spec/16-core-library.md) looks like:

| Type | Layout | Size | Align | Leaves | Niche |
|---|---|---|---|---|---|
| `Box<T>` | pointer (§6.4.3) | `PS` | `PS` | 1 | value 0 |
| `String` | `ptr` at 0, `len` at `PS`, `cap` at `2·PS` | `3·PS` | `PS` | 3 | `ptr` = 0 |
| `Vec<T>`, `Bytes` | `ptr` at 0, `len` at `PS`, `cap` at `2·PS` | `3·PS` | `PS` | 3 | `ptr` = 0 |

`ptr` is never 0. A `String` or `Vec<T>` with capacity 0 has `ptr = align(T)` and allocates nothing. For a zero-sized `T`, `cap` is `USIZE_MAX` and `ptr = align(T)`. These invariants are maintained by the library implementation in [16-core-library.md](../spec/16-core-library.md) and by the allocator ([10-runtime.md](10-runtime.md) §10.2).

### 6.4.6 Structs, tuples and record variants

A struct (`type P { .. }`), a tuple `(A, B, ..)`, the payload of a tuple variant and the payload of a record variant all use `layout_struct`. Field `i` is the `i`-th field in declaration order (for a tuple, the `i`-th element).

```
layout_struct(F: [Layout], repr: Default | C) -> Layout:
    n := len(F)
    if repr == C:
        order := [0, 1, .., n-1]
    else:
        order := stable_sort([0, 1, .., n-1], key = i -> F[i].align, descending)
    offset := 0
    align  := 1
    for i in order:
        offset     := align_up(offset, F[i].align)
        offsets[i] := offset
        offset     := offset + F[i].size          // checked against MAX_OBJECT
        align      := max(align, F[i].align)
    size      := align_up(offset, align)          // checked against MAX_OBJECT
    inhabited := all(F[i].inhabited for i in 0..n)
    leaves    := concat(shift(F[i].leaves, offsets[i]) for i in order)
    niche     := best_niche([shift(F[i].niche, offsets[i]) for i in 0..n])     // §6.4.11
    return Layout { size, align, Struct { offsets, order }, leaves, niche, inhabited }
```

Properties:

- With the default representation, fields are sorted by decreasing alignment. Every field size is a multiple of its alignment, so `align_up` in the loop never adds padding. The only padding of a default-representation struct is tail padding.
- The sort is stable, so fields of equal alignment keep their declaration order (D-WASM-03).
- A zero-sized field gets an offset like any other field and contributes no bytes and no leaves.
- `()` is the tuple with no fields: size 0, align 1.
- `leaves` is in increasing offset order, because `order` is the memory order.
- `@repr(C)` keeps declaration order and inserts padding by the C rule above (D-WASM-03). It applies to structs. Its effect on enums is in §6.4.12.

Example: `type Rec { a: U8, b: U32, c: U16 }`. The default order is `b` (align 4), `c` (2), `a` (1): `b` at 0, `c` at 4, `a` at 6, size 8, align 4. With `@repr(C)`: `a` at 0, `b` at 4, `c` at 8, size 12, align 4.

### 6.4.7 Arrays

`Array<T, N>` has stride `size(T)`, size `N · size(T)` (checked against `MAX_OBJECT`), and align `align(T)`. Element `i` is at offset `i · size(T)`. The array is inhabited if `N = 0` or `T` is inhabited. Its niche is the niche of element 0 if `N ≥ 1`, and none if `N = 0`. Its leaves are the leaves of element 0, then of element 1, and so on, each shifted by the element offset, so it has `N · leaf_count(T)` leaves. An implementation needs to materialize a leaf list only when it has at most 4 leaves (§6.6). `Array<T, 0>` has size 0 and align `align(T)`.

### 6.4.8 Aliases, distinct types and refined types

- A transparent alias `type X = T` is the type `T`.
- A distinct type `distinct D = B` has exactly the layout of `B`, including leaves and niches.
- A refined type `type R = B where p` has exactly the layout of `B`. The refinement predicate never removes bit patterns from the valid range and never creates niches (N-F-7). For example, `Option<R>` for `type Percentage = U8 where self <= 100` has the layout of `Option<U8>` (2 bytes, tagged), not 1 byte.

### 6.4.9 Function items and function pointers

- Each function item has its own zero-sized type (D-TYPE-09): size 0, align 1, no leaves. Calling a function item is a direct call (§6.8.10.1).
- A function pointer type `fn(..) -> ..`, with or without `unsafe` and with any effects clause, is a scalar of width 4 and class FnIdx on **every** target, including wasm64, with valid range `1 ..= 2^32 - 1`. Its value is an index into table 0 (§6.10.4). Table slot 0 never holds a function, so 0 is the niche and `Option<fn(..)>` is 4 bytes (N-F-8). The table is always a 32-bit table, because `table64` is never used (§6.2.1).

### 6.4.10 Closures

A closure value is a struct whose fields are its captures (D-WASM-03). The capture list and its order are those of the operands of the MIR rvalue `Closure(def, [op])` (impl/04). A capture by borrow is a `borrow T` or `borrow mut T` field (one pointer). A capture by value, as with `move fn`, is a field of the captured type. The layout is `layout_struct(captures, Default)`, so captures are reordered by decreasing alignment like any struct. A closure with no captures is a ZST.

Each closure expression has its own nominal type. There is no `dyn` (D-TYPE-02), so a closure is never boxed or erased by the compiler. Generic code that receives a closure is monomorphized for the closure type, and the call is a direct call to the closure body (§6.8.10.2).

**Anonymous futures** (Stage 5). The future type of an `async fn` or `async` block `b` is a struct whose fields are those of `future_layout(b)` ([04](04-mir.md) §4.10.6) with the instance's arguments substituted: the state (`U32`), the captures, one slot per `.await` and the saved locals, in that field order. Its layout is `layout_struct(fields, Default)`, so the fields are reordered by decreasing alignment like any struct. Fields are never overlapped between states.

### 6.4.11 Niches

The niche of a layout is the scalar inside it with the most unused bit patterns. It is used by niche filling (§6.4.12).

```
niche_of(scalar s at offset o) = Some(Niche { o, s }) if available(s) > 0, else None

best_niche(candidates: [Option<Niche>]) -> Option<Niche>:
    keep the Some entries
    if none: return None
    return the candidate with the largest available(scalar);
           ties: the smallest offset; then the earliest candidate in the list
```

- Primitives and pointers: the niche of their single scalar (§6.4.2, §6.4.3).
- Structs and tuples: `best_niche` over the fields, in declaration order (§6.4.6).
- Arrays: the niche of element 0 (§6.4.7).
- Enums: §6.4.12.
- Float scalars, full-range integers and chunk leaves (§6.4.12) never provide a niche.

### 6.4.12 Enums

An enum `E` has variants `V[0..n)` in declaration order. `discr(v)` is the discriminant value of variant `v` as defined by [05-types.md](../spec/05-types.md). For an enum without explicit discriminants it is the declaration index `0, 1, ..`. Explicit discriminants (`= INT`, `= -INT`) exist only on enums whose variants are all unit variants (spec/03, `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM`).

```
layout_enum(V[0..n), repr: Default | C | Int(I)) -> Layout:
    for v in 0..n:
        payload[v] := layout_struct(fields of v, repr == C ? C : Default)
        absent[v]  := not payload[v].inhabited and payload[v].size == 0
    present := [v in 0..n where not absent[v]]          // increasing declaration index

    // Case A: uninhabited enum
    if present is empty:
        return Layout { size 0, align max(1, max(payload[v].align)), Enum{.., NoTag{None}},
                        leaves [], niche None, inhabited false }

    // Case B: one present variant, no tag
    if repr == Default and len(present) == 1:
        p := present[0]
        return payload[p] with shape Enum{.., NoTag{Some(p)}}, payload_offset(p) = 0

    // Case C: niche filling
    if repr == Default and len(present) >= 2:
        sized := [v in present where payload[v].size > 0]
        if len(sized) == 1:
            d := sized[0]
            others := [v in present where v != d]          // increasing declaration index
            if payload[d].niche is Some(Niche{o, s}) and available(s) >= len(others):
                b := 8 · s.width
                start := (s.valid.end + 1) mod 2^b
                s' := s with valid.end := (s.valid.end + len(others)) mod 2^b
                leaves := payload[d].leaves with the leaf at offset o replaced by s'
                return Layout { size payload[d].size, align payload[d].align,
                                Enum{.., Niche{d, o, s', start, others}},
                                leaves, niche_of(s' at o), inhabited true }
                // every variant has payload_offset 0

    // Case D: tagged
    (lo, hi) := (min, max) of discr(v) over all v in 0..n
    if repr == Int(I):   W := size(I); signed := I is signed      // discriminants fit: checked by spec/05 (DISCRIMINANT_OUT_OF_RANGE)
    elif repr == C:      W := 4; signed := lo < 0
    elif lo >= 0:        W := smallest of 1, 2, 4, 8 with hi < 2^(8W);                    signed := false
    else:                W := smallest of 1, 2, 4, 8 with -2^(8W-1) <= lo and hi < 2^(8W-1); signed := true
    tag := Scalar { W, Int{signed}, valid (lo mod 2^(8W), hi mod 2^(8W)) }
    A   := max(1, max over v in 0..n of payload[v].align)
    P   := align_up(W, A)                  // payload offset of every variant
    Psz := max over v in 0..n of payload[v].size
    align := max(W, A)
    size  := align_up(P + Psz, align)      // checked against MAX_OBJECT
    leaves := [Leaf{0, tag}] ++ [Leaf{P + k·A, Scalar{A, Int{unsigned}, full}} for k in 0 .. ceil(Psz / A)]
    return Layout { size, align, Enum{.., Tagged{tag}}, leaves, niche_of(tag at 0), inhabited true }
```

Rules and properties:

- **Tag.** The tag is the smallest unsigned integer that holds every discriminant (D-WASM-03). D-WASM-03 does not cover negative explicit discriminants. For those, the tag is the smallest signed integer that holds every discriminant, stored in two's complement (N-F-9). The tag is always at offset 0.
- **Union of payloads.** Every variant's payload starts at the same offset `P` (D-WASM-03, N-F-17).
- **Chunk leaves.** The payload area of a tagged enum is described by *chunk leaves*: unsigned integers of width `A` that tile `[P, P + ceil(Psz/A)·A)`. Every scalar leaf of every variant payload lies inside exactly one chunk, because a leaf of width `w` sits at a multiple of `w`, and `w` divides `A`. The last chunk may extend past `P + Psz`, but never past `size`, because `size` is a multiple of `align ≥ A`. Bytes of a chunk outside the active variant's leaves are unspecified. Chunk leaves let a tagged enum be held in Wasm locals and flattened at calls like a struct (§6.5.3, §6.6, N-F-17).
- **Niche filling** applies exactly when D-WASM-03's condition holds: among present variants, exactly one variant `d` has a payload of nonzero size, and its niche has at least as many unused values as there are other present variants. When it applies it is always used. The other present variants are encoded, in increasing declaration index, as the consecutive bit patterns `start, start + 1, ..` that follow the end of the niche scalar's valid range. The enum's own niche is the remainder of that range, so niches nest (`Option<Option<Bool>>` is 1 byte).
- **Absent variants** (N-F-17). A variant is absent if its payload is uninhabited and has size 0, such as `Err(Never)`. An absent variant gets no encoding and is never written, so `Result<U32, Never>` has the layout of `U32`. A variant that is uninhabited but has a payload of nonzero size is treated as present, because MIR may partially initialize its payload before the code diverges.
- **`@repr(C)` on an enum**: always tagged, `W = 4`, payloads in C order. Niche filling and Case B do not apply.
- **`@repr(I)` on an enum, where `I` is an integer primitive**: always tagged with tag type `I`. A discriminant outside the range of `I` is the error `DISCRIMINANT_OUT_OF_RANGE` of [05-types.md](../spec/05-types.md) §5.6.2, reported before the backend runs. Whether a given `@repr` argument is accepted is decided by [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md). This section defines the layout of the arguments `C` and `I8`..`U64`, and of nothing else.
- `Option<T>` and `Result<T, E>` are ordinary enums of the core library and use this algorithm.

**Reading the discriminant** of an enum value. `read_tag` loads (memory) or reads (Wasm locals) the tag or niche scalar.

```
discriminant(E, value) -> i128:
    match encoding(E):
        NoTag{Some(p)}:          return discr(p)                       // no memory access
        NoTag{None}:             unreachable                           // uninhabited
        Tagged{tag}:             x := read_tag(value, 0, tag.width)
                                 return tag.signed ? sign_extend(x) : x
        Niche{d, o, s, start, others}:
                                 x   := read_tag(value, o, s.width)    // unsigned
                                 rel := (x - start) mod 2^(8·s.width)
                                 return rel < len(others) ? discr(others[rel]) : discr(d)
```

**Writing the discriminant** (`SetDiscriminant(place, v)`, and the end of an enum `Aggregate`):

```
set_discriminant(E, place, v):
    match encoding(E):
        NoTag{_}:               nothing
        Tagged{tag}:            store discr(v) as a tag.width-byte integer at offset 0
        Niche{d, o, s, start, others}:
                                if v == d: nothing          // the payload of d determines the discriminant
                                else: store (start + k) mod 2^(8·s.width) at offset o, where others[k] == v
```

For the niche encoding of a variant other than `d`, the payload is zero-sized, so no payload store can overwrite the niche value. For `d`, the payload contains the niche scalar, and writing it makes `discriminant` return `discr(d)` because a valid value of the scalar is never in the niche range.

### 6.4.13 Scalar leaves

The **leaves** of a type are its scalar components, in increasing offset order. They are what the type looks like in Wasm locals and at calls.

| Type | Leaves |
|---|---|
| Primitive, pointer, fn pointer, resource handle | the type itself, at offset 0 |
| ZST (`()`, function item, closure without captures, `Array<T, 0>`, `Never`) | none |
| Struct, tuple, closure, view, `String`, `Vec<T>` | the leaves of the fields, in memory order |
| `Array<T, N>` | the leaves of each element, in index order |
| Enum, Case B | the leaves of the present variant's payload |
| Enum, Case C | the leaves of the dataful payload, with the niche scalar's valid range extended |
| Enum, Case D | the tag leaf, then the chunk leaves |

`leaf_count(T)` is the length of this list. The **Wasm value type** of a leaf is `i32` for an Int, Ptr, FnIdx or Handle leaf of width 1, 2 or 4; `i64` for an Int or Ptr leaf of width 8; and `f32` / `f64` for a Float leaf of width 4 / 8. On wasm64, Ptr leaves have width 8 and are `i64`. FnIdx and Handle leaves are always `i32`.

### 6.4.14 Resource types

- A **resource imported through WIT** is represented by its handle: a scalar of width 4, class Handle, valid range `1 ..= 2^32 - 1`, on every target (D-WASM-03). The Canonical ABI never hands out handle index 0, so 0 is the niche, and `Option<R>` of an imported resource is 4 bytes (N-F-8).
- A **resource defined in SAI** (`resource R { fields }`) is laid out as a default-representation struct of its fields. When it is exported, its Canonical ABI rep is a `Box<R>` pointer (D-WASM-03). Creating and consuming that box when a value crosses the component boundary is component glue, owned by [07-component-lowering.md](07-component-lowering.md).
- `borrow R` is an ordinary borrow (§6.4.3): a pointer to wherever the `R` value lives. For a defined resource that is the struct, and for an exported value that is the boxed struct, so the rep can be used directly as `borrow R`.

### 6.4.15 Uninhabited types

`Never`, `Case A` enums and any struct, tuple or non-empty array that contains an uninhabited field are uninhabited. Their size is computed normally, which is 0 for `Never` and for Case A enums. No value of an uninhabited type exists at run time. Code that would produce one is unreachable, and MIR ends it with `Unreachable` or `Trap`. The backend never emits loads or stores of a value of an uninhabited type of size 0.

### 6.4.16 Layout table

The backend records, for every laid-out type: its size and alignment on the current target, the offset and memory order of every field, the encoding of every enum (tag width and signedness, payload offset, or the dataful variant with niche offset, start and assignment), its niche, and its leaves with their Wasm types. `sai inspect layout` (D-TOOL-03) prints this table. Its output format belongs to [12-semantic-tooling.md](12-semantic-tooling.md) and [13-cli.md](13-cli.md).

### 6.4.17 Layout examples

| Type | wasm32: size / align | wasm64: size / align | Encoding |
|---|---|---|---|
| `Bool` | 1 / 1 | 1 / 1 | scalar `0..=1` |
| `Option<Bool>` | 1 / 1 | 1 / 1 | niche: `None = 2` |
| `Option<Option<Bool>>` | 1 / 1 | 1 / 1 | niche: inner `None = 2`, outer `None = 3` |
| `Option<Char>` | 4 / 4 | 4 / 4 | niche: `None = 0x110000` |
| `Option<I32>` | 8 / 4 | 8 / 4 | tagged: tag `U8` at 0, payload at 4; leaves `i32 i32` |
| `Option<I64>` | 16 / 8 | 16 / 8 | tagged: payload at 8; leaves `i32 i64` |
| `Option<borrow T>` | 4 / 4 | 8 / 8 | niche: `None = 0` |
| `Option<Box<T>>` | 4 / 4 | 8 / 8 | niche: `None = 0` |
| `Option<Option<borrow T>>` | 8 / 4 | 16 / 8 | tagged (the inner niche is used up) |
| `Option<fn(I32) -> I32>` | 4 / 4 | 4 / 4 | niche: `None = 0` |
| `Str`, `Slice<T>` | 8 / 4 | 16 / 8 | `(ptr, len)` |
| `Option<Str>` | 8 / 4 | 16 / 8 | niche: `ptr = 0` |
| `String`, `Vec<T>` | 12 / 4 | 24 / 8 | `(ptr, len, cap)` |
| `Result<I32, Str>` | 12 / 4 | 24 / 8 | tagged: payload at 4 (wasm32) or 8 (wasm64); 3 leaves |
| `Result<U32, Never>` | 4 / 4 | 4 / 4 | Case B: `Err` is absent |
| `enum Color { Red Green Blue }` | 1 / 1 | 1 / 1 | tagged `U8`, valid `0..=2` |
| `Option<Color>` | 1 / 1 | 1 / 1 | niche: `None = 3` |
| `(U8, U32, U16)` | 8 / 4 | 8 / 4 | `.1` at 0, `.2` at 4, `.0` at 6 |
| `Array<U16, 3>` | 6 / 2 | 6 / 2 | stride 2 |
| `()`, `fn` item, closure without captures | 0 / 1 | 0 / 1 | ZST |
| `Never` | 0 / 1 | 0 / 1 | uninhabited |

### 6.4.18 Flags types

A flags type ([spec/13](../spec/13-components-and-wit.md) §13.9.4) with `n` flags, `1 ≤ n ≤ 32`, is laid out as a struct with one field: an unsigned integer of `size(flags)` bytes, which is 1 for `n ≤ 8`, 2 for `n ≤ 16` and 4 for `n ≤ 32` (N-F-90). The struct has the size and alignment of that integer and a Struct shape with the field at offset 0. Its one leaf is an unsigned Int scalar of that width.

- **Bits.** Bit `i` (value `2^i`) is set exactly when flag `i`, in declaration order, is in the set ([16](16-interfaces-and-wit.md) §16.9.2).
- **Valid range.** The valid range is full, and a flags type has no niche. A value that crosses the component boundary can have bits `n` and above set, and the operations of [16](16-interfaces-and-wit.md) §16.9.6 ignore them, so such a value is valid.
- **Canonical ABI.** This layout is the Canonical ABI layout of the WIT `flags` type, so [07](07-component-lowering.md) §7.2.4 and §7.4.7 need no conversion. It is the one place where the internal layout is fixed to match the Canonical ABI ([07](07-component-lowering.md) §7.9.1).

| Type | wasm32: size / align | wasm64: size / align | Encoding |
|---|---|---|---|
| flags type with 3 flags | 1 / 1 | 1 / 1 | `U8`, full range |
| flags type with 9 flags | 2 / 2 | 2 / 2 | `U16`, full range |
| flags type with 32 flags | 4 / 4 | 4 / 4 | `U32`, full range |
| `Option<F>`, `F` with 3 flags | 2 / 1 | 2 / 1 | tagged: tag `U8` at 0, payload at 1 |

---

## 6.5 Values in Wasm locals

### 6.5.1 Storage classes of MIR locals

Every MIR local of a Mono body gets exactly one storage class. The class decides where the value lives.

| Class | Storage | Address |
|---|---|---|
| `Zst` | none | `align(T)` when borrowed (§6.4.3) |
| `Reg` | `leaf_count(T)` Wasm locals, one per leaf | none; the local is never borrowed |
| `Frame` | a slot in the function's shadow-stack frame (§6.7) | `$fp + slot_offset` |
| `Indirect` | memory owned by the caller: a by-pointer parameter, or `_0` returned through sret (§6.6) | the value of a Wasm parameter |

Algorithm `classify(body)`:

```
for each local l:
    T := type(l)
    if size(T) == 0:                                   class[l] := Zst;      continue
    if l is a parameter (1 <= l <= arg_count) and pass_mode(T) == Indirect:
                                                       class[l] := Indirect; continue
    if l == _0 and pass_mode(return type) == Indirect: class[l] := Indirect; continue
    class[l] := Reg
for each statement and terminator of body, for each place p = (l, projs) in it:
    if class[l] is Zst or Indirect: continue
    let k := the index of the first projection that leaves l's own storage: a Deref, including
             the Deref of a view, whose viewed sequence is not in l (§6.8.1.1);
             len(projs) if there is none
    let own := projs[0 .. k]                           // projections inside l's own storage
    if p is the operand of Borrow(..) or RawPtr(..) and size(type of p) > 0 and k == len(projs):
        class[l] := Frame                              // the address of l's storage is taken
    if own contains Index(_):
        class[l] := Frame                              // a dynamic offset into l's storage
    if leaf_count(type(l)) > 4:
        class[l] := Frame
```

A parameter of class `Frame` is passed as usual (flattened, §6.6) and stored into its slot in the prologue (§6.7.3). Drop flags created by drop elaboration are ordinary `Bool` locals and are normally `Reg`.

A borrow through a `Deref` (`borrow (*r).f`) takes the address of memory that `r` points to, not of `r`, so it does not change the class of `r`.

### 6.5.2 Canonical form of narrow scalars

A leaf of width 1 or 2, and every `Bool`, `Char` and `Byte`, is held in an `i32` in **canonical form** (N-F-10):

- signed `I8`, `I16`: sign-extended to 32 bits;
- unsigned `U8`, `U16`, `Byte`: zero-extended to 32 bits;
- `Bool`: exactly 0 or 1;
- `Char`: the code point, zero-extended.

Every `i32` that holds such a value is canonical at every point where the value is stored into a Wasm local, passed to or returned from a function, or stored to memory. Loads produce canonical form directly (`i32.load8_s` / `load8_u` / `load16_s` / `load16_u`). An operation whose raw `i32` result may not be canonical re-canonicalizes it: `i32.extend8_s` / `i32.extend16_s` for signed types, `i32.and 0xFF` / `0xFFFF` for unsigned types. §6.8.5 lists where this happens.

Chunk leaves (§6.4.12) have no canonical form. Their bytes outside the active variant are unspecified. No SAI operation compares raw chunks, because equality on enums is field-wise (spec/05).

### 6.5.3 Leaf-wise operations

For a `Reg` local, a place without `Deref` selects either:

- a **leaf range**: a struct field, tuple field or array element of a struct-like value is a contiguous run of the leaves, found through the field offsets; or
- a **chunk extract**: a field of a variant payload of a Case D enum lies inside one chunk leaf. Its value is `(chunk >> (8 · (field_offset - chunk_offset))) & mask(width)`, re-canonicalized (§6.5.2), and reinterpreted for floats (`f32.reinterpret_i32` / `f64.reinterpret_i64`; for an `f32` inside an 8-byte chunk, `i32.wrap_i64` first). Writing such a field clears the field's bits in the chunk and ORs in the new bits.

For a Case C enum held in `Reg`, the dataful variant's fields are leaf ranges like a struct's, and the niche scalar is one of those leaves.

Copying a `Reg` value copies its leaves, one `local.get`/`local.set` per leaf.

---

## 6.6 Internal calling convention

The internal calling convention is compiler-private (D-WASM-04, draft §40). It applies to every call between SAI functions inside one module, including calls through function pointers. The boundary conventions are different: core `export fn` and `extern "wasm"` (§6.6.7), and the Canonical ABI ([07-component-lowering.md](07-component-lowering.md)).

### 6.6.1 Pass modes

```
pass_mode(T) =
    Ignore    if leaf_count(T) == 0        // ZSTs, including Never and ()
    Direct    if 1 <= leaf_count(T) <= 4   // flattened into the leaves
    Indirect  if leaf_count(T) >= 5        // passed by address
```

The threshold of 4 leaves is fixed by D-WASM-04 and is the same on every target and in every profile.

A `borrow T` argument is always passed as the pointer (one `ptr` leaf). The permission of D-WASM-04 to pass a borrow of a small Copy type by value is not used by the reference backend (N-F-11).

### 6.6.2 Signature lowering

```
lower_signature(params: [Type], ret: Type) -> (wasm_params: [ValType], wasm_results: [ValType]):
    wasm_params  := []
    wasm_results := []
    match pass_mode(ret):
        Ignore:   nothing
        Direct:   wasm_results := wasm types of leaves(ret)
        Indirect: wasm_params.push(ptr)                  // the sret pointer is the first parameter
    for T in params:
        match pass_mode(T):
            Ignore:   nothing
            Direct:   wasm_params.extend(wasm types of leaves(T))
            Indirect: wasm_params.push(ptr)
    return (wasm_params, wasm_results)
```

- A return type of `Never` or `()` has no results.
- A function item and a function pointer with the same SAI signature lower to the same Wasm type. A function item coerced to a function pointer needs no thunk.
- The hidden location parameter `_l` of a forwarding body ([04](04-mir.md) §4.13.7) is its last MIR parameter, a pointer with one `ptr` leaf, so it is the last Wasm parameter. A function pointer never refers to a forwarding body: impl/04 reifies such a function through a `ReifyLocation` shim with the declared signature, so a function pointer type and its targets always agree.
- A closure body takes its environment as its first parameter: `borrow Env` for `Fn`, `borrow mut Env` for `FnMut`, `Env` for `FnOnce` (impl/04 supplies the body with that parameter). An environment of a closure without captures is a ZST, so it is `Ignore`, and the body has the signature of the corresponding function pointer type.

### 6.6.3 Direct arguments and results

- A `Direct` argument is passed as its leaves, in leaf order, each as a Wasm argument.
- A `Direct` result is returned as its leaves, in leaf order, with multi-value. The caller receives them on the operand stack and assigns them to the destination in reverse leaf order (the last leaf is on top).

### 6.6.4 Indirect arguments

The caller passes the address of memory that holds the argument value. The callee owns that memory for the duration of the call, may read and write it, and never uses it after it returns.

```
indirect_arg_address(op):
    if op == Move(place) and place is a whole local (no projections) of class Frame:
        return $fp + slot_offset(place.local)       // the caller does not read a moved local again
    if op == Move(place) and place is a whole local of class Indirect:
        return the pointer parameter of that local  // forward the caller's own indirect argument
    otherwise:
        t := a fresh call-temporary slot in the caller's frame (§6.7.2)
        copy the value of op into t (§6.8.13)
        return $fp + slot_offset(t)
```

### 6.6.5 Indirect results (sret)

The caller passes the address of the memory that receives the result as the first Wasm argument. The callee writes the result there through its `_0`, which has class `Indirect`, and returns no Wasm results.

```
sret_address(dest, args):
    if dest is a whole local of class Frame and no operand in args mentions dest.local:
        return $fp + slot_offset(dest.local)
    if dest is a whole local of class Indirect and no operand in args mentions dest.local:
        return the pointer parameter of dest.local
    otherwise:
        t := a fresh call-temporary slot; after the call, copy t into dest (§6.8.13)
        return $fp + slot_offset(t)
```

### 6.6.6 The stack pointer and frames

- `__sp` is global 0, a mutable global of type `ptr` (§6.3.6). It holds the lowest address of the most recent live frame. Frames grow downward.
- **The callee allocates and frees its own frame.** A function with frame size `F > 0` (§6.7) runs this prologue before any other code:

```wat
global.get $__sp
i32.const 8 + F            ;; i64.const on wasm64; a compile-time constant
i32.lt_u
if
  call $__sai_stack_overflow   ;; or only `unreachable`, see below
  unreachable
end
global.get $__sp
i32.const F
i32.sub
local.tee $fp
global.set $__sp
```

  The test is `__sp < 8 + F` (unsigned), which is `__sp - F < 8` from D-WASM-02 written without wrap-around. It is emitted in every profile.
- Before every `return`, and at the end of the function body, the epilogue restores the stack pointer: `local.get $fp`, `ptr.const F`, `ptr.add`, `global.set $__sp`. A function with `F = 0` never reads or writes `__sp`.
- Nothing restores `__sp` on a trap. A trap ends the instance (D-ERR-01), so the value no longer matters.
- **Stack overflow reporting.** `$__sai_stack_overflow` is the runtime function of [10-runtime.md](10-runtime.md) §10.5.6. It reports "stack overflow" through the panic channel and traps. It is called when the program has a panic reporting channel (§6.8.12.1). Otherwise the prologue contains only `unreachable` inside the `if`.
- **Recursion without frames.** A function with `F = 0` does not touch the shadow stack. Unbounded recursion of such functions exhausts the engine's call stack, which the engine reports as a trap. That trap is not a panic and is never reported by SAI.
- Frame sizes may differ between profiles, because inlining and SROA change them ([09-optimization-and-size.md](09-optimization-and-size.md)). Running out of stack is a resource limit, like the engine's call stack, and does not make programs mean different things in different profiles (C-03).

### 6.6.7 Boundary conventions: `export fn` and `extern "wasm"`

A core `export fn` (D-MOD-02) and a function of an `extern "wasm"` block (D-UNSAFE-01) cross the boundary of the core module. Their parameter and result types are restricted to **boundary scalars** (D-WASM-04):

| SAI type | Wasm type | Value check when the value comes from the host |
|---|---|---|
| `I8`, `I16` | `i32` | must equal the sign extension of its low 8 or 16 bits |
| `U8`, `U16`, `Byte` | `i32` | must be `< 2^8` or `< 2^16` |
| `I32`, `U32` | `i32` | none |
| `I64`, `U64`, `Int`, `UInt` | `i64` | none |
| `ISize`, `USize` | `ptr` | none |
| `F32`, `F64` | `f32`, `f64` | none |
| `Bool` | `i32` | must be 0 or 1 |
| `Char` | `i32` | must be `<= 0x10FFFF` and not in `0xD800..=0xDFFF` |
| a distinct type whose base is a boundary scalar and which has no refinement | as its base | as its base |
| `()` (result only) | no result | — |

Any other parameter or result type is the error `EXPORT_FN_NON_SCALAR`. This includes refined types, enums, structs, views and pointers. The check applies:

- to every function of an `extern "wasm"` block, in every build, because the restriction is part of the declaration (D-UNSAFE-01);
- to every `export fn` that is actually exported, that is, every `export fn` of the root package when the artifact is a core module ([04-modules-and-names.md](../spec/04-modules-and-names.md) §4.6.6, N-A-15; [targets-and-profiles.md](../targets/targets-and-profiles.md) §4). In a component build, `export fn` items are not exported ([13-components-and-wit.md](../spec/13-components-and-wit.md) §13.6.4, N-D-46), so their signatures are not restricted. The functions of the exported world are lifted by [07-component-lowering.md](07-component-lowering.md) through the Canonical ABI and may use any WIT-mapped type.

Rich types cross the boundary only through a component build.

Values that come from the host are checked by the rules above (D-CONTRACT-01: a failure at the trust boundary traps):

- **Export.** The **entry** of an `export fn f` is `Shim(BoundaryEntry(f))` if impl/04 created it, which validates refinements and invariants and checks the clauses that are not proven, and `f` otherwise ([04](04-mir.md) §4.12.5). If any parameter needs a value check, the backend emits a wrapper function `<symbol>$export` that checks each such parameter, executes `unreachable` if a check fails, and otherwise calls the entry with the same arguments. The export points to the wrapper. Without value checks, the export points to the entry directly.
- **Import.** If the result needs a check, the check follows each call to the import. A failure executes `unreachable`.
- A failed check is a trap, not a panic. It is not reported, in every profile (N-F-12).

The export name is the function name, or the string of `@export_name("..")`. The import field name is the function name, or the string of `@import_name("..")`. The import module name is the string after `module` (spec/03 §3.10).

---

## 6.7 Frames and Wasm locals

### 6.7.1 Frame contents

The shadow-stack frame of a function holds:

1. one slot for every local of class `Frame` (§6.5.1);
2. one slot for every **call temporary**: a copy of an `Indirect` argument (§6.6.4) or a temporary sret destination (§6.6.5);
3. one slot for every **transmute temporary** (§6.8.8);
4. in `Report` mode, one **site temporary** if the function raises a panic whose location is the hidden parameter `_l` (§6.8.12.2). All such panic sequences of the function share it, because a panic sequence never returns. Its site, for the order of §6.7.2, is the first of them in block and statement order.

Call, transmute and site temporaries are decided by a scan of the MIR before any code is emitted, because the rules of §6.6.4, §6.6.5 and §6.8.8 depend only on the MIR. Each temporary belongs to exactly one call site or conversion.

### 6.7.2 Slot assignment

```
frame_layout(body) -> (slot_offset, F):
    slots := [Frame-class locals in increasing local index]
             ++ [temporaries in order of their site: block index, then statement index,
                 then operand index]
    stable_sort(slots, key = s -> align(type(s)), descending)
    off := 0
    for s in slots:
        off := align_up(off, align(type(s)))
        slot_offset[s] := off
        off := off + size(type(s))
    F := align_up(off, 8)
```

- `F` is a multiple of 8 and `__sp` is always 8-aligned, so every slot is aligned for its type (the largest alignment is 8).
- Slots are never shared, even between locals whose storage ranges (`StorageLive` to `StorageDead`) do not overlap, in every profile (N-F-13). Frame size still differs between profiles when the optimizer changes the MIR (§6.6.6).
- A frame larger than `S / 4` (256 KiB) is the info `LARGE_STACK_FRAME`. It is an info, not a warning, because frame sizes depend on the optimizer: `--deny-warnings` must not make acceptance depend on optimization (decisions §0.1, [impl/09](09-optimization-and-size.md) §9.2.2 item 3).

### 6.7.3 Prologue

The prologue of a function body is, in order:

1. If `F > 0`: the stack check and frame allocation of §6.6.6. The frame base is held in the Wasm local `$fp`.
2. For every parameter of class `Frame`, in parameter order: store its leaves from the Wasm parameters into its slot.

Frame memory is not zeroed. MIR never reads a local before initializing it (impl/05), so the initial content of a slot is never observed.

### 6.7.4 Wasm locals

The Wasm parameters are those of the lowered signature (§6.6.2). They are named after the MIR parameters: `$<name>` for a single-leaf parameter and `$<name>.<k>` for leaf `k` of a multi-leaf one, `$sret` for the sret pointer. A `Reg` parameter uses its Wasm parameters directly as its leaf locals.

The declared Wasm locals follow, in this order (N-F-13):

1. `$fp` (type `ptr`), if `F > 0`;
2. the leaf locals of every non-parameter `Reg` local, in increasing MIR local index, leaves in leaf order;
3. the scratch locals the lowering needs (§6.8), in order of first use in emission order.

A scratch local is never reused for another purpose within a function, so the local list depends only on the MIR. Consecutive locals of the same Wasm type are grouped into one declaration entry, as the binary format requires.

---

## 6.8 Lowering MIR

The backend lowers each MIR block into a straight-line instruction sequence (the block's **code**) followed by a **control terminator**: `Goto(t)`, `If(cond, t, f)`, `Switch(..)` (§6.9.6), `Return` or `Halt` (the code already ended with `unreachable`). §6.9 turns these blocks into structured control flow.

### 6.8.1 Places

#### 6.8.1.1 Place references

Evaluating a place yields a `PlaceRef`:

```
PlaceRef = Zst
         | Leaves  { local, first: u32, count: u32, extract: Option<ChunkField> }   // part of a Reg local
         | Memory  { base: WasmLocal | FrameBase, offset: u64 }                     // address = base + offset
         | DerivedView { ptr: WasmLocal, len: WasmLocal }                           // viewed sequence, or a Subslice of it
```

```
eval_place(p = (l, projs)) -> PlaceRef:
    cur := match class(l):
               Zst      -> Zst
               Reg      -> Leaves(l, 0, leaf_count(type(l)), None)
               Frame    -> Memory($fp, slot_offset(l))
               Indirect -> Memory(<the Wasm parameter holding l's address>, 0)
    T := type(l)
    for proj in projs:
        if size(type after proj) == 0 and proj is not Deref: cur := Zst
        match proj:
            Field(i):
                o := offset of field i in T; for a field of variant v (after Downcast(v)),
                     o := payload_offset(v) + offset of field i in payload(v)
                Memory(b, off) -> Memory(b, off + o)
                Leaves(..)     -> the leaf range of the field, or a ChunkField (§6.5.3)
            Downcast(v):
                remember v for the next Field; no change to cur
            Deref, T = Str, Slice<E> or SliceMut<E>:   // the viewed sequence *s (04 §4.2.3)
                (vp, vl) := the ptr and len of the view at cur, read into scratch locals
                cur := DerivedView(vp, vl);  T := the viewed sequence of E (Byte for Str)
                continue
            Deref:
                if cur is Leaves of a single ptr leaf: cur := Memory(that Wasm local, 0)
                else: cur := Memory(s, 0) where s := the ptr value read from cur
                (Zst if the pointee is a ZST)
            Index(j), T = Array<E, N>:            // cur is Memory: Index forces class Frame
                cur := Memory(s, 0) where s := base(cur) + off(cur) + value(j) · size(E)
            ConstIndex{o, from_end}, T = Array<E, N>:
                k := from_end ? N - o : o
                Memory(b, off) -> Memory(b, off + k · size(E));  Leaves -> leaf range of element k
            Subslice{from, to}, T = Array<E, N>:
                Memory(b, off) -> Memory(b, off + from · size(E));  Leaves -> leaf range;
                T := Array<E, N - from - to>
            Index(j) / ConstIndex / Subslice, T = a viewed sequence of E:   // cur is DerivedView(vp, vl)
                Index(j):                cur := Memory(s := vp + value(j) · size(E), 0)
                ConstIndex{o, from_end}: k := from_end ? vl - o : o;  cur := Memory(s := vp + k · size(E), 0)
                Subslice{from, to}:      cur := DerivedView(vp + from · size(E), vl - from - to)
                                         // T stays the viewed sequence; read as a value, it is a view
        T := the type after proj
    return cur
```

- `Index`, `ConstIndex` and `Subslice` apply to a place of type `Array<E, N>` (the elements are inside the place) or to the viewed sequence `*s` of a view `s` of type `Slice<E>` or `SliceMut<E>` (the elements are the viewed memory, [04](04-mir.md) §4.2.3). A view is not a pointer to a dynamically sized place, because SAI has no DSTs (D-TYPE-02): the `Deref` of a view reads the view's `ptr` and `len` and yields a `DerivedView`, which is not a value of its own. Indexing through a `borrow Array<E, N>` is `Deref` followed by `Index` (spec/06: `(*p)[0]`).
- `Subslice{from, to}` selects the elements `[from, len - to)`, as produced by slice patterns (`[a, rest @ .., z]` gives `from = 1`, `to = 1`).
- A `DerivedView` can be projected further by `Index` / `ConstIndex` / `Subslice`, measured with `Len`, and borrowed: `Borrow(k, *s)` is the view `(ptr, len)` (§6.8.4, [05](05-borrow-checker.md) F17). A `DerivedView` produced by `Subslice` can also be read, as the view of those positions (§6.8.1.2, [04](04-mir.md) §4.2.3). Assigning to a `DerivedView`, and reading the viewed sequence `*s` itself as a value, are internal compiler errors (`INTERNAL_COMPILER_ERROR`).
- Bounds were checked by MIR `Assert`s before the projection (§6.8.7). The projection itself never checks.
- A computed base `s` is materialized in a fresh scratch local only when the resulting place is accessed by more than one load or store. When it is accessed once, the address expression is emitted inline at that access.
- Multiplication by a stride that is a power of two is emitted as `shl`. A stride of 1 emits no multiplication. On wasm64, an `Index` local of type `USize` is already `i64`.

#### 6.8.1.2 Reading and writing a place

**Reading** a `PlaceRef` of type `T` as a value yields the leaves of `T`:

- `Zst`: no values and no instructions.
- `Leaves`: `local.get` of each leaf local, or the chunk extract of §6.5.3.
- `Memory(b, off)`: one load per leaf, at `b + off + leaf.offset`, with the load of §6.8.1.3. A value with more than 4 leaves is never read into Wasm values. It is copied memory to memory (§6.8.13).
- `DerivedView` (a `Subslice` of a viewed sequence): its two scratch locals, the view of those positions.

**Writing** a value of type `T` to a `PlaceRef`:

- `Zst`: nothing.
- `Leaves`: `local.set` of each leaf local (in reverse order when the values are on the operand stack), or a chunk insert (§6.5.3).
- `Memory(b, off)`: one store per leaf, or a memory copy (§6.8.13).

#### 6.8.1.3 Loads and stores

| Leaf scalar | Load | Store | Alignment hint |
|---|---|---|---|
| Int signed, width 1 | `i32.load8_s` | `i32.store8` | 0 |
| Int unsigned (incl. `Bool`, `Byte`) or chunk, width 1 | `i32.load8_u` | `i32.store8` | 0 |
| Int signed, width 2 | `i32.load16_s` | `i32.store16` | 1 |
| Int unsigned or chunk, width 2 | `i32.load16_u` | `i32.store16` | 1 |
| Int / Ptr / FnIdx / Handle / chunk, width 4 (incl. `Char`) | `i32.load` | `i32.store` | 2 |
| Int / Ptr / chunk, width 8 | `i64.load` | `i64.store` | 3 |
| Float, width 4 | `f32.load` | `f32.store` | 2 |
| Float, width 8 | `f64.load` | `f64.store` | 3 |

The constant part of an address (`off + leaf.offset`) is placed in the `offset` immediate of the memory instruction, so the address operand is only the base. The immediate is a `u32` on wasm32 and a `u64` on wasm64. An address operand never wraps: every valid object lies entirely inside memory.

### 6.8.2 Operands

| Operand | Lowering |
|---|---|
| `Copy(place)` | read the place (§6.8.1.2) |
| `Move(place)` | read the place. The source is not cleared. MIR never reads a moved-from place before it is reinitialized. |
| `Const(c)` | §6.8.14 |

When the consumer needs the value in memory (an `Indirect` argument, or an assignment of a value with more than 4 leaves), an operand is used as a source address instead: the address of its place, or of its static data item for a constant.

### 6.8.3 Statements

| Statement | Lowering |
|---|---|
| `Assign(place, rvalue)` | evaluate the rvalue (§6.8.4) and write it to the place. The destination and the source places of one `Assign` are either identical or disjoint (an impl/04 invariant). Identical places produce no code for `Use(Copy)` and `Use(Move)`. |
| `StorageLive(l)`, `StorageDead(l)` | no code. Frame slots exist for the whole function (§6.7.2). |
| `SetDiscriminant(place, v)` | `set_discriminant` (§6.4.12) on the place |
| `FakeRead(cause, place)` | no code |
| `Nop` | no code |

### 6.8.4 Rvalues

| Rvalue | Lowering |
|---|---|
| `Use(op)` | the operand (§6.8.2) |
| `Borrow(_, kind, place)` | the address of the place: `base + offset` (one `ptr.add` if `offset ≠ 0`, nothing more if `offset = 0`). For a ZST place, `ptr.const align(T)` (§6.4.3). For a viewed sequence or a `Subslice` of it (a `DerivedView`), the view `(ptr, len)`: this is the reborrow of a view ([05](05-borrow-checker.md) F17). The region and the kind (`Shared`, `Mut`, `TwoPhaseMut`) do not affect code. |
| `RawPtr(_, place)` | as `Borrow` |
| `BinaryOp(op, a, b)` | §6.8.5 |
| `CheckedBinaryOp(op, a, b)` | §6.8.6. The result is the pair `(wrapped result, overflow flag)`. |
| `UnaryOp(op, a)` | §6.8.5 |
| `Convert(kind, op)` | §6.8.8 |
| `Aggregate(kind, ops)` | write each operand to the corresponding field place of the destination, in operand order, then for an enum variant run `set_discriminant`. Kinds: tuple, struct, enum variant, array. |
| `Discriminant(place)` | `discriminant` (§6.4.12), produced in the Wasm type of the destination's integer type (sign-extended to `i64` if the destination is 64-bit and the tag is signed). |
| `Len(place)` | `Array<T, N>`: `ptr.const N`. A view: its `len` leaf, or a load at offset `PS`. A viewed sequence or a `Subslice` of it (a `DerivedView`): its `len`. |
| `Repeat(op, n)` | §6.8.13.3 |
| `Closure(def, ops)` | as `Aggregate` of the closure struct (§6.4.10) |

### 6.8.5 Arithmetic, bitwise and comparison operators

`BinaryOp` and `UnaryOp` are total in MIR: every check (overflow, division by zero, shift amount, `-MIN`) is a separate `Assert` that runs before the operation (§6.1.1). On integers, `BinaryOp(WrapAdd | WrapSub | WrapMul)` is the wrapping operation: the operators `+%`, `-%` and `*%`, and the operation that replaces a checked operation whose overflow flag was proven to be 0 ([09-optimization-and-size.md](09-optimization-and-size.md) §9.4.6). `Add`, `Sub` and `Mul` of integers occur only inside `CheckedBinaryOp` ([04-mir.md](04-mir.md) §4.2.4); as `BinaryOp` they are float operations.

Notation: `iN` is `i32` for types of width 1 to 4 and `i64` for width 8. `s`/`u` selects the signed or unsigned instruction by the operand type. `canon` re-canonicalizes a narrow result (§6.5.2) and is a no-op for widths 4 and 8.

| Operation | Integer types | Floats |
|---|---|---|
| `WrapAdd`, `WrapSub`, `WrapMul` (integers); `Add`, `Sub`, `Mul` (floats) | `iN.add` / `sub` / `mul`, then `canon` | `fN.add` / `sub` / `mul` |
| `Div` | `iN.div_s` / `div_u` | `fN.div` |
| `Rem` | `iN.rem_s` / `rem_u` (`MIN % -1` is 0, as spec/06 requires) | not built in (spec/06) |
| `BitAnd`, `BitOr`, `BitXor` | `iN.and` / `or` / `xor` | — |
| `Shl` | `iN.shl` with the amount (for `i64`, `i64.extend_i32_u` of the `U32` amount), then `canon` | — |
| `Shr` | `iN.shr_s` / `shr_u` with the amount as for `Shl` | — |
| `Eq`, `Ne` | `iN.eq` / `ne` | `fN.eq` / `ne` |
| `Lt`, `Le`, `Gt`, `Ge` | `iN.lt_s` / `lt_u` and so on | `fN.lt` / `le` / `gt` / `ge` |
| `Offset` (pointer plus element count, unsafe intrinsics) | `ptr + n · size(T)` | — |
| `Neg` | `0 - a` (`iN.sub`), then `canon` | `fN.neg` |
| `Not` on integers and `Byte` | `iN.xor` with `-1` for signed types and 64-bit types, with `0xFF` / `0xFFFF` / `0xFFFF_FFFF` for unsigned types of width 1 / 2 / 4 | — |
| `Not` on `Bool` (`not a`) | `i32.eqz` | — |

- `Bool`, `Char` and `Byte` compare unsigned. Comparisons produce `0` or `1`, which is canonical `Bool`.
- The narrow signed `Div` result is already canonical, because `MIN / -1` was excluded by an `Assert`. Narrow `Shr` of a canonical value is canonical.
- Wasm `div` traps on a zero divisor and on signed `MIN / -1`. Both are excluded by the preceding `Assert`s, so these traps are unreachable. They are not relied on.
- Float operations follow Wasm semantics. NaN bit patterns are unspecified (D-TYPE-06).

### 6.8.6 Checked arithmetic

`CheckedBinaryOp(op, a, b)` produces the wrapped result `r` and an overflow flag `o` (an `i32`, 0 or 1). MIR then asserts that `o` is 0 (`Assert{cond: o, expected: false, kind: Overflow(op)}`) and uses `r`. Each sequence below computes both into scratch locals. All sequences are exact for every input.

| Op | Type | `r` | `o` |
|---|---|---|---|
| Add | signed, width 4 or 8 | `a + b` | `((r ^ a) & (r ^ b)) <s 0` |
| Add | unsigned, width 4 or 8 | `a + b` | `r <u a` |
| Sub | signed, width 4 or 8 | `a - b` | `((a ^ b) & (a ^ r)) <s 0` |
| Sub | unsigned, width 4 or 8 | `a - b` | `a <u b` |
| Add, Sub, Mul | width 1 or 2 | `x := a op b` in `i32` (exact, because 16-bit products fit in 32 bits); `r := canon(x)` | `r != x` |
| Mul | signed, width 4 | `p := i64.mul(extend_s a, extend_s b)`; `r := i32.wrap(p)` | `p != i64.extend_i32_s(r)` |
| Mul | unsigned, width 4 | `p := i64.mul(extend_u a, extend_u b)`; `r := i32.wrap(p)` | `(p >>u 32) != 0` |
| Mul | unsigned, width 8 | `r := a * b` | `a != 0 and (r /u a) != b` |
| Mul | signed, width 8 | `r := a * b` | `a == -1 ? b == MIN : (a != 0 and (r /s a) != b)` |

`ISize` / `USize` use the width-4 rows on wasm32 and the width-8 rows on wasm64.

The signed 64-bit multiply never executes `i64.div_s(MIN, -1)`: the `a == -1` case is tested first, and the division runs only in the arm where `a` is neither `-1` nor `0`. The test is emitted as nested `if (result i32)` instructions inside the block's code. These are the only blocks with a non-empty result type, and they are not part of the CFG of §6.9.

### 6.8.7 Bounds, division and shift checks

MIR expresses every check of spec/06 as an `Assert` whose condition is an ordinary MIR value:

| Check | Condition computed by MIR | `AssertKind` |
|---|---|---|
| index in bounds | `Lt(i, Len(base))` | `BoundsCheck` (operands: `i`, the length) |
| divisor nonzero | `Ne(b, 0)` | `DivByZero` / `RemByZero` |
| no `MIN / -1` (signed) | `not (Eq(a, MIN) and Eq(b, -1))` | `Overflow(Div)` |
| shift amount | `Lt(s, w)` (`w` = bit width) | `ShiftOverflow` |
| no `-MIN` | `Ne(a, MIN)` | `Overflow(Neg)` |
| `+ - *` | flag of `CheckedBinaryOp` | `Overflow(op)` |

The backend lowers each `Assert` as in §6.8.10.7. It never adds a check that MIR does not contain, and never drops one. Checks are removed only by MIR optimizations that prove them redundant ([09-optimization-and-size.md](09-optimization-and-size.md) §9.4.6, D-CONTRACT-01).

### 6.8.8 Conversions

`Convert(kind, op)` covers the conversion functions of D-TYPE-06 and the compiler-generated coercions. If impl/04 represents a conversion function as a call to a core intrinsic instead, the intrinsic is lowered exactly as the matching kind below. Let `S` be the source type and `T` the target type, with bit widths `ws` and `wt`.

| Kind | Meaning | Lowering |
|---|---|---|
| `IntWiden` | `T.from(x)` between integers (lossless) | `ws ≤ 32 < wt`: `i64.extend_i32_s` / `_u` by the signedness of `S`. Otherwise no instruction, because the canonical form of the value in `S` is its canonical form in `T`. |
| `IntWrap` | `x.wrapping_to<T>()` | `ws = 64 > wt`: `i32.wrap_i64`, then `canon` for `T`. `ws ≤ 32 < wt`: extend by the signedness of `S`. Same Wasm type: `canon` for `T` (for example `I8 → U8`: `i32.and 0xFF`; `U8 → I8`: `i32.extend8_s`). |
| `IntSaturate` | `x.saturating_to<T>()` between integers | clamp in the domain of `S`: if `min(T) > min(S)`, `x := x < min(T) ? min(T) : x`; if `max(T) < max(S)`, `x := x > max(T) ? max(T) : x`, comparing with the signedness of `S` and using `select`; then `IntWrap` to `T`. Both constants are representable in `S`. |
| `IntToFloat` | `F32.from(x)`, `F64.from(x)` | `fN.convert_iM_s` / `_u` by the signedness of `S`. Wasm rounds to nearest, ties to even. |
| `FloatToInt` | `x.trunc_to<T>()`, `x.saturating_to<T>()` from a float | `wt = 64`: `i64.trunc_sat_fN_s` / `_u`. `wt ≤ 32`: `i32.trunc_sat_fN_s` / `_u`, then for `wt < 32` the `IntSaturate` clamp to `T`. NaN gives 0. If spec/16 makes a case panic, MIR contains the `Assert`. |
| `FloatPromote` / `FloatDemote` | `F64.from(f)`, `f.trunc_to<F32>()` or the spec/16 name | `f64.promote_f32` / `f32.demote_f64` |
| `Bitcast` | same-width reinterpretation (`Byte ↔ U8`, float bits ↔ integer) | no instruction between integers. `f32.reinterpret_i32`, `i32.reinterpret_f32` and the 64-bit forms between floats and integers. |
| `FnItemToFnPtr(f)` | function item to `fn(..)`. A closure never coerces to a function pointer ([spec/05](../spec/05-types.md) §5.5.3), so there is no closure kind | `i32.const table_index(f)` (§6.10.4). The operand is a ZST and emits nothing. |
| `ArrayToSlice` | `borrow Array<E, N>` (or `borrow mut`) to `Slice<E>` (or `SliceMut<E>`) | `(ptr, ptr.const N)` |
| `PtrToPtr` | between `borrow T`, `Ptr<T>`, `PtrMut<U>` | no instruction |
| `PtrToAddr`, `AddrToPtr` | `Ptr<T> ↔ USize` (unsafe) | no instruction |
| `Transmute` | `core.mem.transmute` (unsafe; equal sizes checked earlier) | if `S` and `T` each have one leaf of the same width: `Bitcast`. Otherwise, write the value to a transmute temporary in the frame (§6.7.1) as `S` and read it back as `T`. |

### 6.8.9 Function calls: callee kinds

The `func` operand of a `Call` terminator is one of:

| Callee | Recognized by | Lowering |
|---|---|---|
| An instance with a Mono body | `Const(Instance(i))` where `i` is an item, a shim or a synthetic glue or runtime body ([04](04-mir.md) §4.13.5) | `call $f` with the function index of `i` (§6.10.3) |
| A closure body | `Const(Instance(Item(c)))` of a closure `c`: monomorphization resolves and untuples a call through `Fn.call`, `FnMut.call_mut` or `FnOnce.call_once` ([04](04-mir.md) §4.3.4) | `call` of the closure body with the environment as first argument (§6.6.2) |
| An `extern "wasm"` function | `Const(Instance(Item(d)))` where `d` is a function of an `extern "wasm"` block | `call` of the import, followed by the result check of §6.6.7 |
| A component import | `Const(Instance(Item(d)))` where `d` is a function of an imported WIT interface | `call` of the import glue of `d` ([07-component-lowering.md](07-component-lowering.md) §7.7.2), whose symbol is `glue:<module>#<field>` (§6.10.2) |
| A core import | `Const(Instance(CoreImport { module, field, sig }))`, only in a synthetic body ([04](04-mir.md) §4.13.9) | `call` of the import `(module, field)` with the core type of `sig` (§6.10.3) |
| An intrinsic | `Const(Instance(Item(d)))` where `d` is `size_of`, `align_of`, `zeroed`, `core.panic.panic` or `core.panic.unreachable` | §6.8.13.4, §6.8.12.2 |
| A runtime intrinsic | `Const(Instance(RuntimeIntrinsic(s)))` ([04](04-mir.md) §4.3.1) | §6.8.15 |
| A function pointer | `Copy` of a place of type `fn(..)` | `call_indirect (type $t) ` on table 0, with the table index as the last operand, where `$t` is the type of the lowered signature |

### 6.8.10 Terminators

#### 6.8.10.1 `Goto`, `Unreachable`, `Return`

- `Goto{target}`: control terminator `Goto(target)`.
- `Unreachable`: `unreachable`; control terminator `Halt`.
- `Return`: if `_0` is `Direct`, read `_0` (its Wasm locals, or its frame slot for a `Frame`-class `_0`) onto the operand stack. Then the epilogue (§6.6.6) and `return`. Control terminator `Return`. If `_0` is `Indirect`, it already lives at the sret address.

#### 6.8.10.2 `Call`

```
lower_call(Call{func, args, dest, target}):
    sig := lower_signature(callee parameter types, callee return type)
    if pass_mode(ret) == Indirect: push sret_address(dest, args)                  // §6.6.5
    for op in args, in order:
        match pass_mode(type(op)):
            Ignore   -> nothing
            Direct   -> push the leaves of op
            Indirect -> push indirect_arg_address(op)                            // §6.6.4
    if the callee is a function pointer: push the table index
    emit call $f | call_indirect (type $t)
    if the callee is an extern "wasm" import: emit the result check (§6.6.7)
    if pass_mode(ret) == Direct: write the results to dest (§6.8.1.2)
    if the sret address was a temporary: copy it to dest (§6.8.13)
    if ret is uninhabited or target is absent: emit unreachable; control terminator Halt
    else: control terminator Goto(target)
```

The `effects` of a `Call` produce no code.

#### 6.8.10.3 `SwitchInt`

`SwitchInt{discr, targets, otherwise}` produces the control terminator `Switch(d, targets, otherwise)`, which §6.9 normalizes (§6.9.1) and lowers to `if`, `br_if` or `br_table` (§6.9.5, §6.9.6). `d` is the Wasm local of `discr` when `discr` is a `Copy` or `Move` of a whole `Reg` local with one leaf. Otherwise `d` is a fresh scratch local, and the block's code ends with the evaluation of `discr` and a `local.set d`.

#### 6.8.10.4 `Trap`

`Trap{kind}` emits `unreachable` (control terminator `Halt`) in every profile, with no report. `Trap` is the MIR form of traps that are not panics: the value checks of boundary scalars (§6.6.7, N-F-12) and an explicit trap intrinsic. The validation of refinements, invariants and `requires` at the trust boundary is checked SAI code, and its failures are panics ([spec/10](../spec/10-errors-and-panics.md) §10.7.1, [spec/09](../spec/09-contracts-and-refinements.md) §9.7.3). Panics are `Assert`s or calls of the panic lang items (§6.8.12).

#### 6.8.10.5 Tail calls (opt-in)

With the `tail-call` feature enabled, a `Call` is emitted as `return_call` (or `return_call_indirect`) when all of the following hold (N-F-1):

1. the caller has `F = 0`;
2. `dest` is the whole local `_0`;
3. the `target` block consists only of `StorageDead` and `Nop` statements and a `Return`;
4. the callee is not an `extern "wasm"` import that needs a result check;
5. the lowered result types of the callee equal those of the caller, and if the return is `Indirect`, the caller forwards its own `$sret` as the callee's sret address.

Otherwise, and always without the feature, the call is an ordinary `call`.

#### 6.8.10.6 `Drop`

`Drop{place, target}`: if the type of the place has no drop glue, the terminator is `Goto(target)`. Otherwise the backend calls the drop glue instance `drop<T>` (§6.1.1), passing the value of the place as a moved argument: its leaves if `Direct`, or the address of the place itself if `Indirect`, because the place is dead after the drop and the glue may use its memory. Then `Goto(target)`. Drop flags were turned into control flow by drop elaboration, so a `Drop` that is reached always drops. Glue with a location parameter is never the glue of a `Drop`, because impl/04 turns those drops into calls (§6.1.1).

#### 6.8.10.7 `Assert`

`Assert{cond, expected, kind, target}`:

```wat
local.get $cond          ;; the condition, a Bool
i32.eqz                  ;; only if expected == true
if
  ;; panic sequence for `kind` (§6.8.12)
end
```

followed by the control terminator `Goto(target)`. The panic sequence ends with `unreachable`, so the `if` has no `else`.

### 6.8.11 Drop glue

Drop glue bodies are ordinary Mono MIR bodies supplied by impl/04 and are lowered like any other function. Their symbol is `drop<T>` (§6.10.2). The order in which they drop fields and elements is the order of spec/07 (D-OWN-05), and it is already explicit in the MIR. `Box<T>` glue drops the pointee and then calls the allocator's free function ([10-runtime.md](10-runtime.md) §10.2).

### 6.8.12 Panics

#### 6.8.12.1 Panic mode

Each build has one **panic mode** (N-F-14):

| Mode | When | Panic sequence |
|---|---|---|
| `Report` | profile `dev` or `release`, **and** a reporting channel exists ([10-runtime.md](10-runtime.md) §10.5.3): in a component build, `wasi:cli/stderr` is already in the import set `Imp` ([07-component-lowering.md](07-component-lowering.md) §7.11.8, N-D-59); when the artifact is a core module, the root package declares a `@panic_hook` import; in a test harness, always, through the harness channel ([10-runtime.md](10-runtime.md) §10.5.3) | call the runtime panic entry, then `unreachable` |
| `TrapOnly` | profile `tiny` (D-ERR-01, D-TGT-02), or no reporting channel exists | `unreachable` only |

Panic reporting never adds an import or a capability. A WASI program that never writes to standard error therefore traps without a report, in every profile.

In `TrapOnly` mode no panic site data, panic messages, formatter or reporting code is linked. This is what keeps `export fn add` free of runtime code on `wasm32-core` (draft §2.3).

#### 6.8.12.2 Panic sequence in `Report` mode

A panic is an `Assert` (§6.8.10.7), a call of `core.panic.panic(msg, loc)` or a call of `core.panic.unreachable(loc)` ([04](04-mir.md) §4.2.6, §4.13.7). Its sequence is:

```wat
<address of the panic site>         ;; a static PanicSite, or the site temporary
ptr.const 0 | <first detail operand>
ptr.const 0 | <second detail operand>
call $__sai_panic
unreachable
```

- **Form.** The site has the form and fixed text of [10-runtime.md](10-runtime.md) §10.5.2: `Bounds` for `BoundsCheck`, `Dynamic` for `core.panic.panic`, and `Text` with the text of its kind for every other `AssertKind` and for `core.panic.unreachable`.
- **Detail operands.** The index and the length for `BoundsCheck`, the `(ptr, len)` leaves of `msg` for `core.panic.panic`, and `0, 0` otherwise.
- **Location.** The location of an `Assert` follows the `location` of its scope ([04](04-mir.md) §4.2.7): `Static` is the start of its `SourceInfo.span`, and `Caller(_l)` is the record that `_l` points to. The location of a panic intrinsic is its last argument: `Const(Location(s))` is the start of `s`, and `Copy(_l)` is the record that `_l` points to.
- **Static site.** When the location is known at compile time, the site is a static data item `PanicSite` with that location, its form and its text (§6.8.14.2), and the first instruction is `ptr.const` of its address.
- **Run-time site.** When the location is `_l`, the sequence fills the site temporary of the function (§6.7.1): `file_ptr` and `file_len` are the two leaves of field 0 of `*_l`, `line` and `column` are fields 1 and 2, and `text_ptr`, `text_len` and `form` are constants. The first instruction is the address of the temporary ([10-runtime.md](10-runtime.md) §10.5.2, run-time locations).
- `$__sai_panic` never returns. The trailing `unreachable` makes that explicit to the validator and the engine.
- The `PanicSite` record, the message format and the reporting channels are in [10-runtime.md](10-runtime.md) §10.5. Two static sites with the same bytes are one data item (§6.8.14.3).

In `TrapOnly` mode the whole sequence is `unreachable`, and no site temporary exists. The message and location operands of a panic intrinsic call are still evaluated by MIR before the call, as the program says. Removing their computation when it has no observable effect is an optimization ([09-optimization-and-size.md](09-optimization-and-size.md)).

### 6.8.13 Memory copies and fills

#### 6.8.13.1 Copying a value

```
copy_value(dst: Memory, src: Memory or Const data, T):
    if size(T) == 0: nothing
    elif leaf_count(T) <= 4:
        load every leaf of src into scratch locals, then store every leaf to dst
    elif profile == release and size(T) <= 64 and align(T) >= 4:
        W := align(T) == 8 ? 8 : 4
        load the size(T) / W words of src into scratch locals, then store them to dst
        (a tail of 4 bytes when W = 8 and size(T) mod 8 = 4 uses one i32 word)
    else:
        memory.copy(dst address, src address, size(T))
```

All loads come before all stores, so the copy is correct for identical places. Copies inside the runtime use `memory.copy` directly. The size threshold and the profile rule are fixed (N-F-15). Profiles may differ in code shape but never in the copied bytes.

#### 6.8.13.2 Copying into or out of `Reg`

A value with at most 4 leaves moves between `Reg` locals and memory leaf by leaf. A `Reg` value never has more than 4 leaves (§6.5.1).

#### 6.8.13.3 `Repeat(op, n)`

`Repeat(op, n)` produces `Array<T, n>`:

- `n = 0` or `size(T) = 0`: nothing.
- The destination is `Reg` (at most 4 leaves in total): write each element's leaves.
- `op` is a constant whose bytes are all zero: `memory.fill(dst, 0, n · size(T))`.
- `size(T) = 1`: `memory.fill(dst, value, n)`.
- `n ≤ 4`: write the element `n` times.
- Otherwise: write element 0, then double the initialized prefix:

```
done := 1
while done < n:                       // a Wasm loop over scratch locals
    k := min(done, n - done)
    memory.copy(dst + done · size(T), dst, k · size(T))
    done := done + k
```

#### 6.8.13.4 Memory intrinsics

The intrinsics of `core.mem` that impl/04 leaves to the backend ([04](04-mir.md) §4.13.9, [spec/16](../spec/16-core-library.md) §16.12.1) and the two memory runtime intrinsics ([10-runtime.md](10-runtime.md) §10.1.2) lower without a runtime function (D-WASM-01). Each is a `Call` whose result is written to `dest`, followed by `Goto(target)`:

| Intrinsic | Lowering |
|---|---|
| `size_of<T>()`, `align_of<T>()` | `ptr.const` of the layout value (§6.4) |
| `zeroed<T>()` | the value of `T` whose bytes are all zero: nothing for a ZST, a zero constant (`i32.const 0`, `i64.const 0`, `f32.const 0`, `f64.const 0`) for each leaf when `dest` is a `Reg` local, and `memory.fill(dest address, 0, size(T))` otherwise |
| `rt.copy(dst, src, n)` | `memory.copy(dst, src, n)` |
| `rt.fill(dst, byte, n)` | `memory.fill(dst, byte, n)` |

`memory.copy` has memmove semantics, so overlapping and non-overlapping copies share one lowering. Library code copies and fills memory through `Ptr.read` and `PtrMut.write` ([spec/16](../spec/16-core-library.md) §16.12.2), which impl/04 lowers to places ([04](04-mir.md) §4.13.9), so no other memory intrinsic reaches the backend.

### 6.8.14 Constants and static data

#### 6.8.14.1 Scalar and small constants

- A scalar constant is `i32.const`, `i64.const`, `f32.const` or `f64.const` with the exact bit pattern of the constant, in canonical form (§6.5.2). Float constants are emitted from their bit pattern, so NaN payloads are preserved as given.
- A constant of a ZST emits nothing.
- A constant aggregate with at most 4 leaves is emitted as one constant per leaf, without static data.
- An `Instance` constant is zero-sized and emits nothing. A `Call` uses it to name its callee (§6.8.9), and `FnItemToFnPtr` to name the function whose table index it produces (§6.8.8).
- `Location(s)` is `ptr.const` of the address of the static data item of its location record (§6.8.14.2).
- `Alloc(id)` is read from the static data item of the allocation: its leaves are loaded from that item, or the item's address is used as the source address (§6.8.2).

#### 6.8.14.2 Static data items

The following become static data items (§6.3.5):

- string literals: the UTF-8 bytes, alignment 1, used as `(ptr.const addr, ptr.const len)`;
- byte-string literals: the bytes, alignment 1, as `Slice<Byte>`;
- constant aggregates with more than 4 leaves, and constant arrays used through a place (for example, indexed by a non-constant index): the bytes of the value in its layout, padding bytes zero, alignment `align(T)`, with relocations for pointers inside the value (addresses of other items);
- the static allocations of [04](04-mir.md) §4.13.6: for `Const(v)`, the bytes of `v` in the layout of `ty`, padding bytes zero, alignment `align(ty)`, with a relocation for each pointer inside the value, such as the data item of a `Str` leaf; for `Location { file, line, column }`, the record `(Str, U32, U32)` in its layout, with a relocation to the file string;
- panic site records and their file-name strings in `Report` mode (§6.8.12).

A data item of length 0 is not emitted. Its address is its alignment, as for every ZST (§6.4.3).

Static data is read-only. SAI has no mutable static state (D-CAP-02), and a borrow of a constant is a borrow of a temporary (spec/07 §7.5.7), so generated code never writes to static data. A constant operand that must be passed `Indirect` is first copied to a call temporary (§6.6.4).

#### 6.8.14.3 Deduplication and order

Two data items are merged when they have the same bytes, the same alignment and the same relocations. The merged item keeps the position of the first occurrence. Items are collected in emission order: functions in the order of §6.10.3, and within a function in block index order, statement order and operand order. Partial overlaps (suffix merging) are not used (N-F-16). Body lowering only records which items a function references; the item list is formed at linking (§6.1.4 step 6) from the functions that remain after identical code folding and final dead-code elimination ([09-optimization-and-size.md](09-optimization-and-size.md) §9.5, §9.6.4). A function removed there contributes no data item.

### 6.8.15 Runtime functions and intrinsics

Generated code calls only the following runtime functions of [10-runtime.md](10-runtime.md), and only when the program reaches them:

| Symbol | Called by | Signature (wasm32; `i64` for `ptr` on wasm64) |
|---|---|---|
| `__sai_panic` | panic sequences in `Report` mode (§6.8.12) | `(site: ptr, a: ptr, b: ptr) -> ()`, never returns |
| `__sai_stack_overflow` | prologues in `Report` mode (§6.6.6) | `() -> ()`, never returns |
| `__sai_alloc`, `__sai_realloc`, `__sai_free` | the allocation intrinsics of spec/16 §16.15.1 | [10-runtime.md](10-runtime.md) §10.2.2 |

A call of a **runtime intrinsic** `rt.*` ([04](04-mir.md) §4.3.1) is lowered to exactly the instruction sequence of the table of [10-runtime.md](10-runtime.md) §10.1.2, with its operands in order, and has no function index. These callees occur only in runtime bodies, and `rt.copy` and `rt.fill` also in glue bodies ([04](04-mir.md) §4.11.5 rule 6).

Runtime functions are MIR bodies built by the compiler ([10-runtime.md](10-runtime.md) §10.1.2), lowered by this chapter like any other code, and removed by item-level DCE when unreachable ([09-optimization-and-size.md](09-optimization-and-size.md) §9.6). Arenas (`region` blocks, D-OWN-07) are library code of `sai:alloc` that calls the allocator functions; they add no runtime function ([10-runtime.md](10-runtime.md) §10.4).

---

## 6.9 Structured control flow

Wasm has only structured control flow: `block`, `loop`, `if`, `br`, `br_if`, `br_table` and `return`. This section turns the Wasm CFG produced by §6.8 into structured code. The algorithm is the one of N. Ramsey, "Beyond Relooper" (ICFP 2022), with a fixed choice at every point where that algorithm leaves one open, and with a preliminary step that makes irreducible CFGs reducible (N-F-18). The output depends only on the Wasm CFG (§6.1.4).

### 6.9.1 The Wasm CFG

The input of this section is, for one function body:

- the **nodes**: one node per MIR block that is reachable from `bb0` through terminator edges. Step 4 of §6.1.4 lowers only reachable blocks, so unreachable blocks produce no code and no static data;
- for each node, its **code** (§6.8) and its **control terminator**;
- the **entry** node, the node of `bb0`.

| Control terminator | Successors, in **successor order** | Code at the end of the node |
|---|---|---|
| `Goto(t)` | `[t]` | — |
| `If(t, f)` | `[t, f]` | the code ends by pushing the `i32` condition: nonzero means `t` |
| `Switch(d, entries, o)` | the distinct targets of `entries` in entry order, then `o` if it is not among them | — (the discriminant is in the scratch local `d`) |
| `Return` | `[]` | the result values are on the operand stack (§6.8.10.1) |
| `Halt` | `[]` | the code ends with `unreachable` |

`entries` is a list of `(value, target)` pairs with distinct values. Before anything else, every node's terminator is normalized:

1. In a `Switch`, the entries whose target is `o` are removed, and the remaining entries are sorted by value, compared in the signedness of the type of `d`.
2. A `Switch` with no entries becomes `Goto(o)`.
3. A `Switch` whose discriminant has type `Bool` becomes `If(t1, t0)`, where `t1` is the target of value 1 (or `o` if there is no entry for 1) and `t0` the target of value 0 (or `o`). The node's code is extended with `local.get d`.
4. A `Switch` with exactly one entry `(v, t)` becomes `If(t, o)`, and the node's code is extended with the comparison: `local.get d; i32.eqz` (or `i64.eqz`) if `v = 0`, and otherwise `local.get d; i32.const v; i32.eq` (or the `i64` forms).
5. An `If(t, t)` becomes `Goto(t)`, and the node's code is extended with `drop` to discard the condition.

After normalization, a node has at most one edge to each of its successors. An **edge** is a pair `(u, v)` with `v` a successor of `u`.

### 6.9.2 Order, dominators, back edges and merge nodes

- **Reverse postorder.** A depth-first search starts at the entry and visits the successors of each node in successor order. `rpo(n)` is the position of `n` in the reverse of the postorder of that search (the entry has `rpo = 0`). Every later step that "orders nodes" orders them by increasing `rpo` unless it says otherwise.
- **Dominators.** `n` dominates `m` if every path from the entry to `m` passes through `n`. `idom(m)` is the immediate dominator. The dominator relation is unique, so any correct algorithm may compute it. The reference implementation uses the iterative algorithm of Cooper, Harvey and Kennedy over the reverse postorder. The **dominator tree** has an edge from `idom(m)` to `m` for every node `m` except the entry.
- **Retreating and back edges.** An edge `(u, v)` is **retreating** if `rpo(v) <= rpo(u)` (this includes self-loops). It is a **back edge** if, in addition, `v` dominates `u`. A CFG is **reducible** iff every retreating edge is a back edge. §6.9.3 makes it so.
- **Loop headers.** A node is a **loop header** iff it is the target of at least one back edge.
- **Forward edges** are the edges that are not back edges. A node is a **merge node** iff it has at least two incoming forward edges. Because a node has at most one edge from each predecessor (§6.9.1), this is the same as having at least two distinct forward predecessors.

In a reducible CFG, a node `m` that is not a merge node and not the entry has exactly one forward predecessor `p`, and `p = idom(m)`. Every node `m` that is a child of `x` in the dominator tree is therefore either a successor of `x` reached by a forward edge (a **direct child**) or a merge node (a **merge child**).

### 6.9.3 Irreducible control flow

MIR built from SAI source is always reducible: SAI has no `goto`, and `loop`, `while`, `for`, labeled `break` and `continue` all produce single-entry loops. The MIR passes of [09-optimization-and-size.md](09-optimization-and-size.md) MUST preserve reducibility ([09](09-optimization-and-size.md) §9.2.3 item 2). The backend nevertheless accepts any CFG, so that a pass that breaks this rule produces a larger but correct module instead of a crash (P9). The fix is a dispatcher transformation (N-F-18):

```
make_reducible(cfg):
    fix_region(all nodes, cfg)
    remove the nodes that are no longer reachable; recompute rpo and dominators (§6.9.2)

fix_region(R, cfg):                                   // R: a set of nodes
    sccs := strongly connected components of the subgraph induced by R,
            computed by Tarjan's algorithm with roots taken in increasing rpo
            and successors visited in successor order
    for S in sccs, ordered by the smallest rpo of their nodes:
        if S has one node and no edge to itself: continue
        E := [n in S such that n is the entry or n has a predecessor outside S], ordered by rpo
        if |E| == 1:
            fix_region(S \ E, cfg)                    // E[0] is the loop header
        else:
            H := new node with empty code and terminator
                 Switch($label, [(0, E[0]), .., (k-2, E[k-2])], E[k-1])   where k = |E|
            for every edge (p, E[i]) of cfg, for every p, with p not one of the new nodes:
                B := new node with code `i32.const i; local.set $label` and terminator Goto(H)
                replace the edge (p, E[i]) by (p, B)
            if E contains the entry: a new node `i32.const i; local.set $label; Goto(H)`,
                 where E[i] is the old entry, becomes the entry
            N := the new nodes B whose edge source p is in S
            fix_region(S ∪ N, cfg)                    // H is now the only entry of S ∪ N ∪ {H}
```

- `$label` is one `i32` scratch local per function, shared by all dispatchers of that function. It is declared when the first dispatcher is created.
- New nodes are placed in the successor order of the node whose edge they replace, at the position of the replaced successor.
- After `make_reducible`, every SCC with more than one node, at every level of the recursion, has a single entry. Every retreating edge then targets that entry, which dominates the source, so the CFG is reducible.
- The transformation does not duplicate code. Its cost is one `i32.const` and `local.set` per redirected edge and one dispatch per dispatcher.

### 6.9.4 The structuring algorithm

The algorithm walks the dominator tree. It keeps a **context**: a stack of the enclosing Wasm control frames, innermost first. Each frame is one of:

| Frame | Pushed by | Meaning of a branch to its label |
|---|---|---|
| `LoopHeadedBy(h)` | the `loop` emitted for loop header `h` | continue at `h` |
| `BlockFollowedBy(m)` | the `block` emitted before merge node `m` | continue at `m`, whose code follows the block's `end` |
| `IfThenElse` | an `if` with both arms inline | (never a branch target) |
| `SwitchArm(n)` | a wrapper `block` of a `Switch` (§6.9.6) | continue at the direct child `n`, whose code follows the block's `end` |

The label index of a frame in a `br`, `br_if` or `br_table` is its position in the context, the innermost frame being 0.

```
do_tree(x, ctx):
    ms := merge children of x in the dominator tree, ordered by decreasing rpo
    if x is a loop header:
        emit loop
        node_within(x, ms, LoopHeadedBy(x) : ctx)
        emit end
    else:
        node_within(x, ms, ctx)

node_within(x, ms, ctx):
    if ms is empty:
        emit code(x)
        emit_terminator(x, ctx)                           // §6.9.5, §6.9.6, §6.9.7
    else:
        m := head(ms); rest := tail(ms)                   // m has the highest rpo
        emit block
        node_within(x, rest, BlockFollowedBy(m) : ctx)
        emit end
        do_tree(m, ctx)

branch_target(x, y, ctx) -> Label(i) | Inline:
    if (x, y) is a back edge:   Label(index of LoopHeadedBy(y) in ctx)
    else if y is a merge node:  Label(index of BlockFollowedBy(y) in ctx)
    else:                       Inline                    // y is a direct child of x

do_branch(x, y, ctx):
    match branch_target(x, y, ctx):
        Label(i) -> emit br i
        Inline   -> do_tree(y, ctx)

structure(function):
    make_reducible(cfg)                                   // §6.9.3
    do_tree(entry, [])
    apply the peepholes of §6.9.7
```

- Merge children are placed with the highest `rpo` outermost, so the code of merge nodes appears in increasing `rpo` order, and every forward branch to a merge node `m` comes from code inside the `block` that `m` follows.
- Every `block`, `loop` and `if` emitted by this section has the empty block type `[] -> []`.
- The code emitted for a node always ends with an instruction that does not fall through (`br`, `br_table`, `return`, `unreachable`, or an `if`/`else` whose arms both end that way), before the peepholes of §6.9.7. Control therefore never reaches an `end` of this section by falling through, and the code after a `block`'s `end` is reached only through branches to that block.
- The function body is not a frame. `Return` uses `return`.

### 6.9.5 Two-way branches

For `If(t, f)` at the end of node `x`, the condition is on the operand stack. With `Lt = branch_target(x, t, ctx)` and `Lf = branch_target(x, f, ctx)`:

| `Lt` | `Lf` | Emitted |
|---|---|---|
| `Label(i)` | `Label(j)` | `br_if i` `br j` |
| `Label(i)` | `Inline` | `br_if i`, then `do_tree(f, ctx)` |
| `Inline` | `Label(j)` | `i32.eqz` `br_if j`, then `do_tree(t, ctx)` |
| `Inline` | `Inline` | `if` `do_tree(t, IfThenElse : ctx)` `else` `do_tree(f, IfThenElse : ctx)` `end` |

`Goto(t)` emits `do_branch(x, t, ctx)`.

### 6.9.6 Multi-way branches

For `Switch(d, entries, o)` at the end of node `x`, after normalization `entries` has `n >= 2` entries (§6.9.1). Let `T` be the successors of `x` in successor order and `I = [i_1, .., i_k]` those for which `branch_target` is `Inline`, ordered by increasing `rpo`. The emitted code nests one wrapper `block` per inline target, with the dispatch innermost.

The emitted code is `switch_arms(k)`. `switch_arms(0)` is `<dispatch>`, and for `j >= 1`, `switch_arms(j)` emits `block`, then `switch_arms(j - 1)`, then `end`, then `do_tree(i_j, ctx_j)`, where `ctx_j` is `ctx` with `SwitchArm(i_{j+1}) .. SwitchArm(i_k)` pushed. The dispatch runs inside all `k` frames (none if `k = 0`). The label of a target `y` in the dispatch is the index of `SwitchArm(y)` if `y` is in `I`, and otherwise `i + k`, where `Label(i) = branch_target(x, y, ctx)`.

`<dispatch>` selects between a jump table and a compare chain (N-F-19). Let `lo` and `hi` be the smallest and largest entry values, compared in the signedness of the type of `d`, and `range = hi - lo + 1`, computed exactly.

- **Jump table** if `n >= 4`, `range <= 4 · n`, and `range <= 65536`:

```wat
;; i32 discriminant (all integer types of width <= 4, and Char)
local.get $d
i32.const lo        ;; omitted if lo = 0
i32.sub             ;; omitted if lo = 0
br_table L(lo) L(lo+1) .. L(hi) L(o)
```

```wat
;; i64 discriminant
local.get $d
i64.const lo        ;; omitted if lo = 0
i64.sub             ;; omitted if lo = 0
local.tee $t        ;; an i64 scratch local
i64.const range
i64.ge_u
br_if L(o)
local.get $t
i32.wrap_i64
br_table L(lo) L(lo+1) .. L(hi) L(o)
```

  `L(v)` is the label of the target of the entry with value `v`, or of `o` if there is no such entry. The subtraction wraps, so a value outside `[lo, hi]` becomes an unsigned index `>= range` and takes the default label `L(o)`, for signed and unsigned discriminants alike.

- **Compare chain** otherwise, entries in their sorted order:

```wat
local.get $d
i32.const v1        ;; i64.const for an i64 discriminant
i32.eq              ;; i32.eqz instead of `i32.const 0; i32.eq`
br_if L(v1)
...                 ;; one group per entry
br L(o)
```

### 6.9.7 Returns, halts and peepholes

- `Return` emits the epilogue (§6.6.6) and `return`. The result values are already on the operand stack, below the epilogue's stack effect, which is neutral.
- `Halt` emits nothing more: the code already ends with `unreachable`.

After `do_tree`, three peepholes are applied, in this order, in every profile:

1. A `br 0` that is the last instruction of the body of a `block` is removed. Falling off the end of a block goes to the same place. (This does not apply to `loop`, where `br 0` jumps to the start.)
2. A `return` that is the last instruction of the function body is removed. The results are on the stack, which is what the end of the function body requires.
3. If the function has at least one result and its body now ends with `end` (of a `block`, `loop` or `if`), an `unreachable` is appended. Control never reaches that point (§6.9.4), but the validator types the code after an `end` with the block's empty result type, and the end of the function body requires the result values. `unreachable` makes the operand stack polymorphic. A function without results needs nothing, because the empty stack matches.

No other rewriting of the structured code happens in the backend. Size optimizations of the instruction stream belong to [09-optimization-and-size.md](09-optimization-and-size.md) §9.7.

### 6.9.8 Example

```sai
fn classify(x: I32) -> I32 {
    var r: I32 = 0
    if x < 0 {
        r = -1
    } else if x > 0 {
        r = 1
    }
    r
}
```

The MIR, with `_1 = x`, `_2 = r` and no checks (comparisons and literals need none):

```
bb0: _2 = 0; _3 = Lt(_1, 0); SwitchInt(_3, [(0, bb2)], otherwise: bb1)
bb1: _2 = -1; Goto(bb4)
bb2: _4 = Gt(_1, 0); SwitchInt(_4, [(0, bb4)], otherwise: bb3)
bb3: _2 = 1; Goto(bb4)
bb4: _0 = _2; Return
```

Normalization turns both `SwitchInt` on `Bool` into `If`: `bb0` is `If(bb1, bb2)` and `bb2` is `If(bb3, bb4)`. The depth-first search visits `bb0, bb1, bb4, bb2, bb3`. The postorder is `bb4, bb1, bb3, bb2, bb0`, so the reverse postorder is `bb0 (0), bb2 (1), bb3 (2), bb1 (3), bb4 (4)`. There are no back edges. `bb4` has three forward predecessors and is the only merge node. The dominator tree is `bb0 → {bb1, bb2, bb4}`, `bb2 → {bb3}`. `bb4` is a merge child of `bb0`.

```wat
(func $classify (param $x i32) (result i32)
  (local $_0 i32) (local $r i32) (local $_3 i32) (local $_4 i32)
  block                        ;; BlockFollowedBy(bb4); ctx = [BlockFollowedBy(bb4)]
    ;; bb0
    i32.const 0
    local.set $r
    local.get $x
    i32.const 0
    i32.lt_s
    local.set $_3
    local.get $_3              ;; added by normalization rule 3
    if                         ;; If(bb1, bb2): both arms inline; ctx = [IfThenElse, BlockFollowedBy(bb4)]
      ;; bb1
      i32.const -1
      local.set $r
      br 1                     ;; Goto(bb4): bb4 is a merge node, label 1
    else
      ;; bb2
      local.get $x
      i32.const 0
      i32.gt_s
      local.set $_4
      local.get $_4
      i32.eqz                  ;; If(bb3, bb4): bb3 inline, bb4 Label(1), row 3 of §6.9.5
      br_if 1
      ;; bb3
      i32.const 1
      local.set $r
      br 1                     ;; Goto(bb4)
    end
  end
  ;; bb4
  local.get $r
  local.set $_0
  local.get $_0)               ;; `return` removed by peephole 2
```

The `br 1` instructions stay, because they are inside the `if`, not directly the last instruction of the `block` (peephole 1). The locals `$_3`, `$_4` and `$_0` are there because §6.7.4 declares one Wasm local per leaf of every `Reg` local. Removing such copies is the job of the optimizer, not of the backend: return-place propagation on MIR ([09-optimization-and-size.md](09-optimization-and-size.md) §9.4.4) and the Wasm passes ([09](09-optimization-and-size.md) §9.7). The `release` form of this function is in [09](09-optimization-and-size.md) §9.7.5.

---

## 6.10 Linking and module assembly

### 6.10.1 Whole-program model

The backend produces one core module for the whole program: every reachable Mono function instance of every package compiled from source, the runtime functions of [10-runtime.md](10-runtime.md), and the synthetic glue bodies of [07-component-lowering.md](07-component-lowering.md). There are no object files, no relocations in the output, no separate compilation at the Wasm level, and no dynamic linking (D-TGT-02). A dependency that is used as a component artifact (D-PKG-05) is not linked into the core module; it is composed at the component level ([07-component-lowering.md](07-component-lowering.md)).

**Imports and exports are fixed by reachability, not by optimization** (N-F-20). The set of core imports is the set of `extern "wasm"` functions and WIT imports in the reachable set of [08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.3, plus the imports impl/07 adds for them. That set is computed before optimization and is identical in every profile. An import stays even when optimization removes every call to it, so whether a host can instantiate a module never depends on the profile (C-03). The only exceptions are the imports of the panic reporting channel ([10-runtime.md](10-runtime.md) §10.5.3): the `@panic_hook` import, which is present exactly when the panic mode is `Report` or reachable SAI code calls it ([10-errors-and-panics.md](../spec/10-errors-and-panics.md) §10.6.2 item 2, N-C-80), and, in a component build in `Report` mode, the core imports of the two standard error functions, which never change the component's imports (N-D-59). Function exports are fixed in the same way (§6.10.5). The `memory` export follows `needs_memory` (§6.3.4), which is true whenever the boundary passes pointers, so a host that needs the memory always finds it.

### 6.10.2 Symbols

Every function, global and data item of the module has a **symbol**: a UTF-8 string that is unique within the build and depends only on the program, never on the order of compilation (N-F-20). Symbols are used for ordering (§6.10.3), for the name section (§6.11.2), for DWARF linkage names (§6.12) and for size attribution (§6.11.5).

```text
Symbol        ::= ItemSymbol | WrapperSymbol | DropSymbol | ShimSymbol | GlueSymbol | ImportSymbol | RuntimeSymbol
ItemSymbol    ::= VPath TypeArgs? Nested* Part?
VPath         ::= PackageId '@' Version ModulePart? Component*
TypeArgs      ::= '<' VType ( ', ' VType )* '>'
Nested        ::= '.{closure#' DIGITS '}' | '.{async#' DIGITS '}'
Part          ::= '$requires#' DIGITS | '$ensures#' DIGITS | '$invariant#' DIGITS | '$refinement'
WrapperSymbol ::= ItemSymbol '$export'
DropSymbol    ::= 'drop<' VType '>'
ShimSymbol    ::= ShimName '<' ShimArg ( ', ' ShimArg )* '>'
ShimName      ::= 'checked_entry' | 'boundary_entry' | 'boundary_validate' | 'future_poll'
                | 'refined_forward' | 'refined_try_from' | 'structural_clone' | 'structural_eq'
                | 'structural_cmp' | 'structural_hash' | 'fn_trait_item' | 'closure_adapter'
                | 'reify_location'
ShimArg       ::= ItemSymbol | VType | 'Fn' | 'FnMut' | 'FnOnce' | FILE ':' DIGITS ':' DIGITS
GlueSymbol    ::= 'glue:' GlueRole? CoreName
GlueRole      ::= '[provide]' | '[adapter]'
ImportSymbol  ::= 'import:' ModuleName '#' FieldName
RuntimeSymbol ::= '__sai_' NAME | 'cabi_realloc'
```

- `VPath` is the canonical path of [04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9.1 with `@` and the package's resolved version inserted after the `PackageId`. The version is needed because two compatibility lines of one package may coexist in a build (D-PKG-02, `DUPLICATE_PACKAGE_LINE`). Example: `acme:shop@1.2.0/model.Order.create`. A function of an `extern "wasm"` block has the `VPath` of its canonical path `m.f`.
- `VType` is `CanonType` of spec/04 §4.9.1, with every canonical path written as a `VPath`. `Int` is printed `I64` there, so `f<Int>` and `f<I64>` are the same instance and the same symbol.
- `TypeArgs` lists the arguments of every generic parameter in scope of the instance, outermost first: the parameters of an enclosing impl, then those of the function. It is omitted when there are none.
- `Nested` names a closure body by its index among the closure expressions of the immediately enclosing body, counted from 0 in order of their start offset, and an `async` block in the same way among the `async` blocks. A closure or `async` block has no `TypeArgs` of its own: its instance is determined by the instance of its enclosing function. Example: `acme:app@0.1.0/main.run<I32>.{closure#0}`. The body of an `async fn` is the function's own `ItemSymbol`.
- `Part` names the other bodies of an owner ([04](04-mir.md) §4.2.1): a contract clause by its kind and its index among the clauses of that kind of the function, counted from 0 in declaration order; element `k` of the `invariant` of a type `T` as `ItemSymbol(T)$invariant#k`; the refinement predicate of a refined type `R` as `ItemSymbol(R)$refinement`. A method of a derived impl has the canonical path of the item of the trait impl ([spec/04](../spec/04-modules-and-names.md) §4.9.1), in the module of the type.
- In a `VType`, a closure type and an anonymous future type are written as the `ItemSymbol` of their body.
- `WrapperSymbol` is the export wrapper of §6.6.7. `DropSymbol` is the drop glue instance of `T` (§6.8.11).
- `ShimSymbol` names every other shim of [04](04-mir.md) §4.3.2, by the snake-case name of its kind and its arguments in field order: a `DefId` as the `ItemSymbol` of the item with the instance's arguments, a type as its `VType`, a function trait kind by its name, and the site of `ReifyLocation` as the file string, line and column of its location record ([04](04-mir.md) §4.13.6). Examples: `checked_entry<acme:app@0.1.0/main.f>`, `structural_clone<(I32, Str)>`, `reify_location<sai:core@0.1.0/option.Option.unwrap<I32>, acme:app@0.1.0/src/main.sai:4:9>`.
- A synthetic body `Synthetic { producer, symbol }` ([04](04-mir.md) §4.3.1) has the symbol `symbol` that its producer gives it: a `GlueSymbol` for impl/07 and a `RuntimeSymbol` for impl/10.
- `GlueSymbol` names a synthetic glue body of impl/07 by the core name it serves: `glue:` followed by the core export name for export glue and post-return functions (for example `glue:acme:imaging/processor@0.3.0#resize`), and `glue:` followed by `<module>#<field>` for import glue. The glue of an interface provided within the build ([07](07-component-lowering.md) §7.11.10) inserts a `GlueRole` after `glue:`: `[provide]` for the provider's export glue, post-return functions and destructors, named by the core export name that the provider would have as the root, and `[adapter]` for the adapters and drop adapters, named by the core import name of the function they serve (for example `glue:[adapter]acme:imaging/processor@0.3.0#resize`). A `GlueRole` keeps these symbols distinct from the import and export glue of the same names. `ImportSymbol` names a core import created by impl/07 ([07-component-lowering.md](07-component-lowering.md) §7.10.1).
- `RuntimeSymbol` names the runtime functions and runtime globals of [10-runtime.md](10-runtime.md), with fixed names, and `cabi_realloc` ([10-runtime.md](10-runtime.md) §10.3).
- A static data item has the symbol of the function that first referenced it (§6.3.5, `owner`), followed by `$data` and its index among the data items first referenced by that function, counted from 0 in collection order: `acme:app@0.1.0/main.greet$data0`. `PanicSite` records use `$site` instead of `$data`.

Symbols are compared as byte strings. Two Mono instances with the same symbol are the same instance.

### 6.10.3 Index spaces and their order

All index spaces are ordered deterministically (N-F-20):

| Index space | Order |
|---|---|
| Functions | First the imported functions, ordered by the byte strings of `(module name, field name)`, in that order of comparison. This merges the `extern "wasm"` imports with the imports of [07-component-lowering.md](07-component-lowering.md) §7.10.3 and the `CoreImport` callees of synthetic bodies ([04](04-mir.md) §4.13.9). Then the defined functions, ordered by symbol. |
| Types | The distinct function types, in order of first use: the import types in function index order, then the types of the defined functions in function index order, then the types used by `call_indirect`, in function index order and instruction order within each function. |
| Tables | At most one table, index 0 (§6.10.4). |
| Memories | At most one memory, index 0 (§6.3.7). |
| Globals | `__sp` first, if it exists, then the runtime globals, ordered by symbol (§6.3.6). |
| Data segments | In increasing address order (§6.3.5). |

The function order is the same in every profile. It is not optimized for the size of `call` immediates. A function's index therefore changes only when functions are added or removed, which keeps `sai size` reports and debugging output comparable between builds (P8).

**Merged imports.** Two `extern "wasm"` functions, or `CoreImport` callees, that name the same `(module, field)` pair are one import if their lowered signatures (§6.6.2) are equal. Each call site still applies the result check of its own declaration (§6.6.7). If the lowered signatures differ, the build fails with `CONFLICTING_WASM_IMPORT`, reported at the second declaration in canonical path order, with the first as a secondary label.

### 6.10.4 The function table

The module has a table if and only if at least one function is **address-taken**: it appears in a `FnItemToFnPtr` conversion (§6.8.8) of an emitted function. Static data names no function (§6.3.5).

- Table 0 has element type `funcref`, minimum and maximum `1 + n`, where `n` is the number of address-taken functions. It is not imported and not exported.
- One active element segment for table 0, with offset `i32.const 1`, lists the address-taken functions ordered by symbol. It uses the MVP element segment encoding (flags 0, function indices).
- The **table index** of an address-taken function is `1 +` its position in that list. Slot 0 stays null. It is the niche of function pointers (§6.4.9, N-F-8), and a `call_indirect` through 0 traps, as a call of a null function pointer must.
- `call_indirect` always names table 0 and the type index of the callee's lowered signature. A signature mismatch cannot happen in valid MIR. If it happened, the engine would trap.

If no function is address-taken, the module has no table section and no element section.

### 6.10.5 Exports

| Artifact | Exports |
|---|---|
| core module (`sai build` for `wasm32-core` / `wasm64-core`) | every `export fn` of the root package (its wrapper or its entry, which is `Shim(BoundaryEntry(f))` or `f`, §6.6.7), under its export name; `memory`, if the module has a memory (§6.3.7) |
| core module of a component build | exactly the core exports that [07-component-lowering.md](07-component-lowering.md) §7.10.2 requires, plus `memory` (impl/07 reports that the boundary needs a memory, §6.3.4) |
| test harness (`sai test`) | the entries `test.<k>`, `case.<k>` and `counterexample.<k>` of the selected tests and `memory`, if the module has a memory ([13-cli.md](13-cli.md) §13.6.6.2 item 3); no `export fn` |

- Exports are ordered by the byte strings of their names ([07-component-lowering.md](07-component-lowering.md) §7.10.4 uses the same order).
- In a core-module build, the name `memory` is reserved, whether or not the module ends up with a memory, so that acceptance does not depend on code generation. An `export fn` whose export name is `memory`, or two exports with the same name, is `DUPLICATE_EXPORT_NAME` ([spec/15](../spec/15-attributes-and-lints.md) §15.7), reported at the export name before code generation.
- Every exported function is a root for item-level DCE ([09-optimization-and-size.md](09-optimization-and-size.md) §9.6). Exports are never removed or renamed by optimization.

### 6.10.6 Encoding and section order

The module is encoded with `wasm-encoder` (D-TOOL-05) in the binary format, with minimal-length LEB128 encodings everywhere. Sections appear in this order, each only if it is non-empty:

| # | Section | Content |
|---|---|---|
| 1 | type (1) | §6.10.3 |
| 2 | import (2) | functions only; never memories, tables or globals |
| 3 | function (3) | defined functions in index order |
| 4 | table (4) | §6.10.4 |
| 5 | memory (5) | §6.3.7 |
| 6 | global (6) | §6.3.6 |
| 7 | export (7) | §6.10.5 |
| 8 | element (9) | §6.10.4 |
| 9 | code (10) | function bodies in index order |
| 10 | data (11) | §6.3.5 |
| 11 | custom sections | §6.11, in the order given there |

There is never a start section (§6.2.1) and never a data count section, because `memory.init` and `data.drop` are not used (§6.2.3). Every global is defined with a constant initializer (`ptr.const`). The module is validated with `wasmparser` with exactly the features of §6.2 enabled, after encoding and in every build. A validation failure is `INTERNAL_COMPILER_ERROR` ([11-diagnostics.md](11-diagnostics.md) §11.4.6).

---

## 6.11 Custom sections and side outputs

### 6.11.1 Custom sections by profile

| Custom section | `dev` | `release` | `tiny` |
|---|---|---|---|
| `name` | module, function, local and global names | module and function names | — |
| `producers` | yes | yes | — |
| `target_features` | yes | yes | — |
| `sai.meta` | yes | yes | — |
| DWARF (`.debug_*`, §6.12) | yes | — | — |
| `component-type:sai` (component builds, [07-component-lowering.md](07-component-lowering.md) §7.11.1) | yes | yes | yes |

The order in the module is the order of this table, from top to bottom. `component-type:sai` is last, as impl/07 requires, and is consumed by `wit-component` when the component is wrapped. D-TGT-02's "name section only" for `release` is read as a statement about debug information: `release` has no DWARF and no local names. The `producers`, `target_features` and `sai.meta` sections are metadata, not debug information, and are kept in `release` (N-F-21). `tiny` emits no custom section other than the one that component wrapping consumes, so a `tiny` core module contains only code and data.

No custom section contains a timestamp, an absolute path, a host name, a user name or an environment value ([13-cli.md](13-cli.md) §13.5.3).

### 6.11.2 The `name` section

The `name` section follows the extended name section format, with subsections in increasing id order:

| Subsection (id) | Profiles | Content |
|---|---|---|
| module name (0) | `dev`, `release` | the root package identity with version, for example `acme:image@1.0.0` |
| function names (1) | `dev`, `release` | every function (imported and defined) with its symbol (§6.10.2), in function index order |
| local names (2) | `dev` | for every defined function with at least one parameter or local, in function index order: every Wasm parameter and local, in index order, with the name of §6.7.4 |
| global names (7) | `dev` | `__sp` and the runtime globals, with their symbols |

Local names (§6.7.4) are formed as follows: a MIR local with a source name `n` is `n`, or `n.<k>` for its leaf `k`; a MIR local without a source name (a temporary) is `_<index>`, or `_<index>.<k>`; `$fp` is `fp`, the sret pointer is `sret`, a dispatcher label is `label` (§6.9.3), and scratch locals are `s<k>`, numbered from 0 in declaration order. If two MIR locals have the same source name (shadowing), the later ones in local index order get the suffix `#<n>`, with `n` counted from 1. Name strings are UTF-8 and are never truncated.

### 6.11.3 `producers` and `target_features`

- `producers` (the tool-conventions format) has the field `language` with the single entry `SAI` and the language version, and the field `processed-by` with the entry `sai` and the toolchain version. In a component build, impl/07 adds `wit-component` ([07-component-lowering.md](07-component-lowering.md) §7.11.6).
- `target_features` lists, with the prefix `+`, every feature beyond the Wasm 1.0 MVP that the module actually uses, ordered by name: `bulk-memory` (if `memory.copy` or `memory.fill` occurs), `memory64` (wasm64 targets), `multivalue` (if a function type has more than one result), `nontrapping-fptoint` (if a `trunc_sat` instruction occurs), `sign-ext` (if a sign extension instruction occurs), `simd128` and `tail-call` (if used, §6.2.2). `mutable-globals` and `reference-types` are never listed, because no global is imported or exported and only one table is used.

### 6.11.4 `sai.meta`

`sai.meta` records how the module was built. Its content is one JSON object in the canonical form of [11-diagnostics.md](11-diagnostics.md) §11.9.2 (UTF-8, no insignificant whitespace, members in the order below):

```json
{"format":1,"toolchain":"0.1.0","target":"wasm32-core","profile":"release","package":"acme:image@1.0.0","lock":"sai1:9f2c...","capabilities":["alloc"],"panic":"trap"}
```

| Member | Value |
|---|---|
| `format` | `1` |
| `toolchain` | the toolchain version ([13-cli.md](13-cli.md) §13.3.7) |
| `target`, `profile` | the build's target and profile |
| `package` | the root package identity with version |
| `lock` | the checksum of the bytes of `sai.lock` in the `sai1:` form of D-PKG-03 (lowercase hex), or `null` if the build has no lock file |
| `capabilities` | the capabilities the program uses (the reachable effects mapped to capabilities, D-EFF-03), as names sorted by bytes |
| `panic` | `report` or `trap`, the panic mode (§6.8.12.1) |

The section never affects execution. It lets `sai size` and `sai inspect` show how an artifact was produced, and it lets a reviewer check a published artifact against its lock file.

### 6.11.5 Size attribution table

Every build produces a **size attribution table** as a side output. It is never part of the module. The toolchain stores it in `.sai/cache/` ([13-cli.md](13-cli.md) §13.5.2), and `sai size` reads or recomputes it ([09-optimization-and-size.md](09-optimization-and-size.md) §9.10).

```
AttributionTable {
    module_size: u64,                  // bytes of the module binary
    entries: [Entry],                  // a partition of [0, module_size), in offset order
}
Entry {
    offset: u64, length: u64,          // byte range in the module binary
    section: SectionKind,              // Header | Type | Import | Function | Table | Memory | Global
                                       // | Export | Element | Code | Data | Custom(name)
    owner: Owner,
}
Owner = Function(symbol)               // code bytes of a function body (see below)
      | Inlined(symbol, into: symbol)  // code bytes that came from inlining `symbol` into `into`
      | DataItem(symbol)               // bytes of a data segment that belong to one data item
      | Import(symbol) | Export(name) | Global(symbol) | ElementOf(symbol)
      | FunctionType(index) | SectionOverhead | ZeroFill | Metadata(section name)
```

- **Completeness.** Every byte of the module belongs to exactly one entry. Section ids and size prefixes, vector counts, and the module header are `SectionOverhead`.
- **Code.** The entry of a function body covers its size prefix, its local declarations and its instructions. Instructions are attributed to the Mono instance whose MIR statement or terminator they were lowered from: the function itself, or `Inlined(g, into: f)` for a statement that MIR inlining copied from `g` (the inlining record is kept in MIR source scopes, [09-optimization-and-size.md](09-optimization-and-size.md) §9.4.5). Prologue, epilogue and structuring instructions (`block`, `loop`, `end`, `br`) belong to the function itself.
- **Data.** The bytes of a data segment are attributed to the data items they contain. Padding between items belongs to the following item. Zero runs inside a segment shorter than the split threshold (§6.3.5) are `ZeroFill`.
- **Indices.** A `call` immediate is part of the caller's code, not of the callee.
- **Entries that describe one item.** The function-section entry (the type index) of a defined function is `Function(symbol)`. An import entry is `Import(symbol)`, an export entry `Export(name)`, a global entry `Global(symbol)`, the index of a function in the element segment `ElementOf(symbol)`, and a type-section entry `FunctionType(index)`. The memory and table section entries and the element segment's header are `SectionOverhead`. A custom section is one entry `Metadata(name)`, including its id, size and name.

### 6.11.6 Layout table and boundary metadata

The layout table of §6.4.16 and the boundary metadata of §6.1.3 are in-memory outputs passed to `sai inspect layout` and to impl/07. They are not written into the module.

---

## 6.12 Debug information

### 6.12.1 Policy

| Profile | DWARF |
|---|---|
| `dev` | DWARF 5 line table and subprogram entries |
| `release` | none (function names only, in the `name` section) |
| `tiny` | none |

v0.1 emits no DWARF for variables, types, scopes or inlined subroutines in any profile (N-F-22). Debuggers therefore show source lines, function names and call stacks, but not variable values. This is a deliberate choice: describing frame-class locals and leaf-split register locals needs location expressions for Wasm locals and the shadow stack, and the tool-conventions for those are not stable across debuggers. A later version can add them without changing anything else in this chapter.

### 6.12.2 Sections

`dev` builds emit three custom sections, in this order: `.debug_abbrev`, `.debug_info`, `.debug_line`. They use DWARF 5, the 32-bit DWARF format, and an address size equal to `PS`. Strings are encoded inline (`DW_FORM_string`), so there is no `.debug_str` or `.debug_line_str` section.

**Addresses.** Following the WebAssembly tool conventions, an address is a byte offset from the start of the contents of the code section. The address of a function is the offset of the first byte of its local declarations (after its size prefix).

**`.debug_info`** contains one compile unit for the whole module:

| DIE | Attributes |
|---|---|
| `DW_TAG_compile_unit` | `DW_AT_producer` = `sai <toolchain version>`; `DW_AT_name` = the root package identity with version; `DW_AT_stmt_list` = 0; `DW_AT_low_pc` = 0; `DW_AT_high_pc` (`DW_FORM_udata`, a length) = the size of the code section contents. No `DW_AT_comp_dir` and no `DW_AT_language` (SAI has no registered language code). |
| `DW_TAG_subprogram`, one per defined function in function index order, as children of the unit | `DW_AT_name` = the source name of the item (the last component of its canonical path, or the symbol for synthetic functions); `DW_AT_linkage_name` = the symbol; `DW_AT_low_pc`, `DW_AT_high_pc` (a length, up to and including the final `end`); `DW_AT_decl_file` and `DW_AT_decl_line` of the item's name span, if it has a source span |

**`.debug_line`** has one program for the unit:

- The directory table has one entry, `.`. The file table lists every source file that contributes a row, as a source reference path of [11-diagnostics.md](11-diagnostics.md) §11.5.4 (`src/main.sai`, `dep:sai:http@1.4.2/src/client.sai`), in order of first use, with directory index 0 and no MD5. No absolute path appears.
- Rows: for each function in index order, a row at the function's address with the item's declaration line and column; then, for each MIR statement or terminator in emission order, a row at the address of its first instruction if that instruction exists and its `(file, line, column)` differs from the previous row. Lines and columns are those of [11-diagnostics.md](11-diagnostics.md) §11.5.2 (1-based, column in UTF-8 bytes). Code that came from inlining has the line of the inlined statement. Every row has `is_stmt = true`. One `DW_LNE_end_sequence` closes the program at the end of the code section.
- The header uses `minimum_instruction_length = 1`, `maximum_operations_per_instruction = 1`, `default_is_stmt = 1`, `line_base = -5`, `line_range = 14`, `opcode_base = 13`.

### 6.12.3 What is not emitted

No `.debug_frame` (Wasm has no CFA), no `.debug_aranges`, no location lists, no range lists, and no external DWARF file. A `release` or `tiny` build never carries any `.debug_*` section, so stripping is never needed.

---

## 6.13 Decisions introduced by this chapter

[decisions.md](../design/decisions.md) does not decide the points below. Each is decided here by the principles P1–P9 and is cited in the body by its ID. The N-F IDs are shared by impl/06, impl/09, impl/10, spec/10 and the targets document. Each is unique, and a later addition takes the next free number, so the IDs of one chapter need not be consecutive (N-F-90 follows N-F-22 here).

| ID | Rule | Rationale |
|---|---|---|
| N-F-1 | The Wasm feature set depends only on the target and the opt-in features, never on the profile. `tail-call` and `simd128` are off by default. `tail-call` turns exactly the calls that meet the five conditions of §6.8.10.5 into `return_call`. `simd128` has no effect on code generation in v0.1 and only adds `simd128` to `target_features` (§6.2). | P8: whether an engine can run a module never depends on the profile (C-03). P9: no auto-vectorizer in v0.1. |
| N-F-2 | The shadow stack size `S` is 1 MiB on every target and in every profile, and is not configurable in v0.1 (§6.3.3). | P8: stack overflow happens at the same depth in every profile. P7. |
| N-F-3 | `needs_memory` and `needs_stack` are decided after all bodies are lowered and select one of the three layouts of §6.3.4. Linker symbols are compile-time constants and are never exported. Data segments split at zero runs of at least 16 bytes (§6.3.5). | D-WASM-02 says "only if needed"; P7 (no memory for scalar programs, draft §2.3); P8 (fixed threshold, deterministic output). |
| N-F-4 | A module has at most one memory: minimum `__initial_pages`, no maximum, never imported, never shared, exported as `memory` whenever it exists, on every target (§6.3.7). | P4: a host can read the bytes behind pointers that `@panic_hook` and `extern "wasm"` pass; the Canonical ABI needs it. P9: one rule for all targets. |
| N-F-5 | The `Char` niche is the single range `0x110000..=0xFFFFFFFF`. Surrogate values are never assigned to a variant (§6.4.2). | P9: a `Scalar` has one contiguous valid range; the remaining niche is still larger than any enum needs. |
| N-F-6 | The address of a zero-sized value is the constant `align(T)`. A zero-size allocation returns `align` and allocates nothing (§6.4.3). | P8: pointers stay non-null and aligned without storage; P9. |
| N-F-7 | Refinement predicates never narrow a valid range and never create niches (§6.4.8). | P8, P6: changing a refinement never changes a layout; layout does not depend on predicate analysis (P2). |
| N-F-8 | Function pointers and imported resource handles are 4-byte scalars with niche 0 on every target. Table slot 0 never holds a function (§6.4.9, §6.4.14). | P9: the table is 32-bit on wasm64 too; `Option<fn(..)>` needs no tag. |
| N-F-9 | Negative explicit discriminants use the smallest signed tag that holds every discriminant. `@repr(C)` enums use a 4-byte tag, signed iff some discriminant is negative (§6.4.12). | D-WASM-03 covers only non-negative discriminants; P9 (two's complement, no bias); C interoperability for `@repr(C)`. |
| N-F-10 | Narrow scalars, `Bool`, `Char` and `Byte` are kept in canonical form in Wasm locals, at calls and in memory (§6.5.2). | P9: comparisons and widening need no normalization; P8: boundary checks see a single representation. |
| N-F-11 | The reference backend never passes a borrow by value, although D-WASM-04 permits it (§6.6.1). | P8: address identity of borrowed places is preserved; P9: one rule for every borrow. |
| N-F-12 | A failed value check of an `export fn` parameter or an `extern "wasm"` result traps without a report, in every profile (§6.6.7). | D-CONTRACT-01 (boundary failures trap); P8; P5: no reporting import is added. |
| N-F-13 | Frame slots are never shared. Wasm locals are declared in the fixed order of §6.7.4, and scratch locals are never reused (§6.7). | P8: the output depends only on the MIR; P9. Frame size is reduced by MIR passes, not by the backend. |
| N-F-14 | Panic mode is `Report` iff the profile is `dev` or `release` and a reporting channel exists; otherwise `TrapOnly`. Panic reporting never adds an import or a capability (§6.8.12.1). | P5: no hidden `stderr` import; P4; D-ERR-01. |
| N-F-15 | A value with at most 4 leaves is copied leaf by leaf. In `release`, a value of at most 64 bytes with alignment at least 4 is copied word by word. Every other copy is `memory.copy` (§6.8.13.1). | D-TGT-02 (speed in `release`, size in `tiny`); P8: profiles differ in code shape, never in copied bytes. |
| N-F-16 | Static data items are merged only when bytes, alignment and relocations are equal. The merged item keeps the first position, and there is no suffix merging (§6.8.14.3). | P9; P8: deterministic addresses. |
| N-F-17 | A variant whose payload is uninhabited and zero-sized is absent and has no encoding. All payloads of a tagged enum start at `P = align_up(W, A)`. The payload area is described by chunk leaves of width `A` (§6.4.12). | P9: `Result<T, Never>` is laid out as `T`; enums can live in Wasm locals and be flattened at calls like structs (D-WASM-04). |
| N-F-18 | Control flow is structured by the algorithm of §6.9 ("Beyond Relooper" with fixed choices). An irreducible CFG is first made reducible by dispatcher blocks driven by one `$label` local per function (§6.9.3). | P9: one well-studied algorithm; P8: output depends only on the CFG; robustness against passes that break reducibility. |
| N-F-19 | `br_table` is used iff the switch has at least 4 entries, `range ≤ 4 · n` and `range ≤ 65536`. Otherwise a compare chain is emitted (§6.9.6). | P8: fixed thresholds; bounds the size of jump tables (D-TGT-02 size goals). |
| N-F-20 | The whole program is one module. Symbols follow §6.10.2. Index spaces are ordered by symbol. Imports and exports are fixed by reachability before optimization and are identical in every profile, except the panic channel imports of [10-runtime.md](10-runtime.md) §10.5.3 (spec/10 N-C-80, N-D-59). Same-name imports with different signatures are an error (§6.10). | C-03 and P8: instantiability never depends on the profile; P4; P9 (no dynamic linking, D-TGT-02). |
| N-F-21 | Custom sections per profile are those of §6.11.1. `release` keeps `producers`, `target_features` and `sai.meta`. `tiny` emits none except `component-type:sai`. No section contains a timestamp, absolute path or environment value. | Reading of D-TGT-02 ("name section only" is about debug information); P7 for `tiny`; P8 (reproducible builds). |
| N-F-22 | DWARF is emitted only in `dev`, and contains only the compile unit, subprograms and line tables (§6.12). | P9: the tool conventions for Wasm variable locations are not stable; P7. |
| N-F-90 | A flags type with `n` flags is a one-field struct of an unsigned integer of 1, 2 or 4 bytes (for `n` at most 8, 16 or 32), with bit `i` for flag `i`, a full valid range and no niche (§6.4.18). | impl/07 §7.9.1 requires the Canonical ABI layout for flags (N-D-52). P9: a full range keeps values with unused high bits from the boundary valid. |

---

## 6.14 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `TYPE_TOO_LARGE` | error | The size of a reachable Mono type exceeds `MAX_OBJECT` (§6.4.1). Reported at the use that instantiated the type. |
| `EXPORT_FN_NON_SCALAR` | error | defined in [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.7.4, §12.8. Reported before the backend runs; §6.6.7 relies on it. |
| `LARGE_STACK_FRAME` | info | A function's shadow-stack frame is larger than `S / 4` (256 KiB) (§6.7.2). |
| `CONFLICTING_WASM_IMPORT` | error | defined in [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.7.6. The lowered signatures it compares are those of §6.10.3. |
| `INTERNAL_COMPILER_ERROR` | error | defined in [01-introduction.md](../spec/01-introduction.md) §1.10 and reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6. Used here for Mono MIR that violates §6.1.1, an invalid `DerivedView` use (§6.8.1.1), and a module that fails validation (§6.10.6). |
| `DISCRIMINANT_OUT_OF_RANGE` | error | defined in [05-types.md](../spec/05-types.md) §5.6.2. Reported before the backend runs (§6.4.12). |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | error | defined in [spec/03](../spec/03-grammar.md) §3.6; see §6.4.12 |
| `DUPLICATE_EXPORT_NAME` | error | defined in [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.7. Includes a collision with the reserved export `memory` (§6.10.5). |
