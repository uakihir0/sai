# 10. Runtime

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 1 (panic reporting, stack overflow reporting, initialization); 2 (allocators, arenas); 4 (`cabi_realloc`); 5 (async support, provisional)
**Related:** [decisions.md](../design/decisions.md) (D-WASM-02, D-WASM-05, D-ERR-01, D-OWN-07, D-TGT-01, D-TGT-02, D-CAP-02, D-ASYNC-01, D-THREAD-01) · [04-mir.md](04-mir.md) · [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) · [07-component-lowering.md](07-component-lowering.md) · [09-optimization-and-size.md](09-optimization-and-size.md) · [07-ownership-and-borrowing.md](../spec/07-ownership-and-borrowing.md) · [08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) · [10-errors-and-panics.md](../spec/10-errors-and-panics.md) · [11-async-and-concurrency.md](../spec/11-async-and-concurrency.md) · [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) · [16-core-library.md](../spec/16-core-library.md) · [01-package-contract.md](../package/01-package-contract.md) · [targets-and-profiles.md](../targets/targets-and-profiles.md)

This chapter specifies the runtime of the reference implementation: the small amount of code and state that the compiler adds to a program besides the program's own functions. It covers how runtime functions are built and linked, the runtime state and globals, the global allocator interface and its three implementations (the default allocator, the tiny allocator, and an allocator package named by `allocator <pkg>`), `cabi_realloc`, the implementation of arenas in `sai:alloc`, the formatting and delivery of panic reports, stack overflow and out-of-memory handling, initialization and the instance lifecycle, and the runtime side of async support. The semantics of panics (kinds, messages, locations, channels, `@panic_hook`) belong to [10-errors-and-panics.md](../spec/10-errors-and-panics.md); this chapter implements them. The memory layout, the placement of runtime state and the lowering of panic sequences belong to [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md). Canonical ABI glue belongs to [07-component-lowering.md](07-component-lowering.md); this chapter owns the allocator behind `cabi_realloc`. The surface of `Arena`, `Box`, `Vec` and the allocation primitives belongs to [16-core-library.md](../spec/16-core-library.md). Executors are library code of `sai:async` ([11-async-and-concurrency.md](../spec/11-async-and-concurrency.md) §11.9.3). Nothing in this chapter is a stable ABI, except the interface that an allocator package implements (§10.2.7) and the bytes passed to a reporting channel (§10.5).

---

## 10.1 Overview

### 10.1.1 Components of the runtime

The runtime consists of the following components. Each one is linked only when the program reaches it (§10.1.4). A program that reaches none of them contains no runtime code, no runtime state and no runtime globals (D-WASM-05, draft §2.3).

| Component | Symbols | Linked when | Section |
|---|---|---|---|
| Allocator | `__sai_alloc`, `__sai_realloc`, `__sai_free` and their helpers | a call of an allocation primitive remains after item-level DCE | §10.2 |
| Out-of-memory path | `__sai_oom` | an allocator is linked | §10.2.6 |
| `cabi_realloc` | `cabi_realloc` | impl/07 exports it (N-D-49) | §10.3 |
| Panic reporter | `__sai_panic` and its helpers | the panic mode is `Report` and a panic sequence remains after DCE | §10.5 |
| Stack overflow path | `__sai_stack_overflow` | the panic mode is `Report` and some function has a shadow-stack frame | §10.5.6 |
| In-module handle table | `__sai_handle_new`, `__sai_handle_rep`, `__sai_handle_lend`, `__sai_handle_unlend`, `__sai_handle_move`, `__sai_handle_drop` and their helper `__sai_handle_grow` | a call of one of them remains in the provision glue of impl/07 ([07](07-component-lowering.md) §7.6.5) | §10.3.3 |

The following are **not** runtime components, although they run code that the compiler did not take from the program's own source:

- Drop glue (impl/04, [06](06-wasm-lowering-and-layout.md) §6.8.11) and Canonical ABI glue ([07](07-component-lowering.md)) are synthetic bodies of other chapters.
- Arenas are library code of `sai:alloc` (§10.4). This chapter fixes their algorithm for the reference library, but they are compiled like every other library function.
- Executors, wakers and the reactor are library code of `sai:async` (§10.8).
- The shadow stack has no runtime function. The `__sp` global and the prologue check are emitted by impl/06 (§10.6).

### 10.1.2 How runtime functions are built

Runtime functions are **compiler-built Mono MIR bodies** (N-F-23). They are not parsed from SAI source, because they need operations that SAI code cannot express: runtime globals (SAI has no mutable statics, D-CAP-02), `memory.grow`, and the addresses of linker symbols.

- The compiler builds them after effect analysis and monomorphization, in the MIR phase `Mono` of [04-mir.md](04-mir.md), in the same way impl/07 builds glue. The query `runtime_body(symbol)` of `sai_runtime` returns the body of the runtime function `symbol` for the build configuration of the session, which fixes the allocator (§10.2.1). impl/04 reaches it through the hook `MirHooks::runtime_body` ([04](04-mir.md) §4.1.4) and collects each runtime function as the instance `Synthetic { Runtime, symbol }` ([04](04-mir.md) §4.13.1): as the callee of an allocation primitive ([04](04-mir.md) §4.13.9) or of another runtime body, and as an artifact root for `cabi_realloc` when it is exported and for `__sai_panic` and `__sai_stack_overflow` in `Report` mode, whose calls impl/06 emits. They are not borrow-checked and are not subject to effect analysis. Their effects are accounted for by the reachability edge "the allocator, if `alloc ∈ Own(f)`" of [08](../spec/08-effects-and-capabilities.md) §8.12.3, and by panics having no effect ([10](../spec/10-errors-and-panics.md) §10.4.1).
- They enter the pipeline before MIR optimization, so the passes of [09-optimization-and-size.md](09-optimization-and-size.md) §9.4 apply to them, including inlining where [09](09-optimization-and-size.md) §9.4.5 allows it: the allocator entry points are inlined only into other runtime bodies, and the panic and out-of-memory entry points never. They are lowered by impl/06 like any other body.
- Their symbols are fixed names starting with `__sai_`, plus `cabi_realloc` ([06](06-wasm-lowering-and-layout.md) §6.10.2, `RuntimeSymbol`). A package cannot define an item with such a symbol, because every item symbol starts with a package identity.
- They use only ordinary MIR statements and the **runtime intrinsics** below. A runtime intrinsic is a `Call` of a reserved callee that impl/04 accepts only in runtime bodies, except that the glue bodies of impl/07 may also call `rt.copy` and `rt.fill` ([04](04-mir.md) §4.11.5 rule 6). impl/06 lowers each one to exactly the listed instruction sequence.

| Runtime intrinsic | Lowering (wasm32; `i64` forms on wasm64) |
|---|---|
| `rt.memory_size() -> USize` | `memory.size 0` (pages) |
| `rt.memory_grow(pages: USize) -> USize` | `memory.grow 0`; the all-ones value means failure |
| `rt.global_get(g)`, `rt.global_set(g, v)` | `global.get` / `global.set` of the runtime global `g` (§10.1.3) |
| `rt.addr(s) -> USize` | `ptr.const` of the address of the runtime-state item or linker symbol `s` ([06](06-wasm-lowering-and-layout.md) §6.3.6) |
| `rt.trap() -> Never` | `unreachable` |
| `rt.clz(x: USize) -> USize` | `i32.clz` / `i64.clz` |
| `rt.load(a: USize) -> USize`, `rt.store(a: USize, v: USize)` | `i32.load` / `i32.store` (`i64` on wasm64), alignment `PS`, offset 0 |
| `rt.load32(a)`, `rt.store32(a, v)`, `rt.load8(a)`, `rt.store8(a, v)` | `i32.load`, `i32.store`, `i32.load8_u`, `i32.store8` |
| `rt.copy(dst, src, n)`, `rt.fill(dst, byte, n)` | `memory.copy`, `memory.fill` |

Rules that every runtime body obeys (N-F-23):

1. **No shadow-stack frame.** Every local of a runtime body is a `Reg` local ([06](06-wasm-lowering-and-layout.md) §6.5.1), so its frame size is 0 and it has no prologue check. The runtime therefore works when the shadow stack is exhausted, which the stack overflow path needs (§10.5.6). If a runtime body ever had a frame, impl/06 reports `INTERNAL_COMPILER_ERROR`.
2. **No calls of program code.** A runtime body calls only runtime functions, runtime intrinsics, the functions of an allocator package (§10.2.7) and the core imports of the reporting channel (§10.5.3). It never calls a `Drop`, `Format` or other trait impl, a closure, or a library function ([10](../spec/10-errors-and-panics.md) §10.5.2 item 3).
3. **No checks of its own.** Runtime bodies contain no MIR `Assert`. Every arithmetic operation in them is preceded by the explicit comparisons of its algorithm, which make overflow impossible, and the operations are emitted as wrapping operations.
4. **Recursion.** No runtime function calls itself, directly or indirectly, except the bounded call of `__sai_panic` from `__sai_oom` and `__sai_stack_overflow`.

### 10.1.3 Runtime state and runtime globals

**Runtime state** items are zero-initialized memory areas placed after static data, in the order of this table, by [06](06-wasm-lowering-and-layout.md) §6.3.6 (N-F-24). An item is present only when its component is linked. impl/07 adds its own items after these ([07](07-component-lowering.md) §7.5.3).

| # | Item | Size | Alignment | Present when | Content |
|---|---|---|---|---|---|
| 1 | `__sai_alloc_heads` | `9 · PS` | `PS` | the default allocator is linked | the heads of the 8 size-class free lists, then the head of the large free list (§10.2.4) |
| 2 | `__sai_tiny_head` | `PS` | `PS` | the tiny allocator is linked | the head of the free list (§10.2.5) |
| 3 | `__sai_custom_state` | `align_up(STATE_SIZE, 8)` | 8 | an allocator package is linked and `STATE_SIZE > 0` | the allocator package's state (§10.2.7) |
| 4 | `__sai_panic_buf` | 1024 | 8 | the panic reporter is linked | the report buffer (§10.5.5) |
| 5 | `__sai_panic_ret` | 12 | 4 | the panic reporter is linked and the channel is standard error | the return area of `blocking-write-and-flush` (§10.5.4) |
| 6 | `__sai_handles` | `4 · PS` | `PS` | the in-module handle table is linked | the header of the handle table (§10.3.3, N-F-95) |

At most one of items 1 to 3 is present, because a build has one allocator (§10.2.1).

**Runtime globals** follow `__sp` in the global index space, ordered by symbol ([06](06-wasm-lowering-and-layout.md) §6.3.6, §6.10.3):

| Global | Type | Initial value | Present when | Content |
|---|---|---|---|---|
| `__sai_heap_top` | mutable `ptr` | `__heap_base` | an allocator is linked | the lowest address that no allocator block or granted region covers (§10.2.6) |
| `__sai_panicking` | mutable `i32` | 0 | the panic reporter is linked | 1 while a report is being built or delivered (§10.5.8) |

`__sai_heap_top` sorts before `__sai_panicking`, so when both exist their indices are `k` and `k + 1`, where `k` is 1 if `__sp` exists and 0 otherwise.

### 10.1.4 Linking

Whether a component is linked is decided after item-level DCE ([09-optimization-and-size.md](09-optimization-and-size.md) §9.6) and before Wasm assembly ([06](06-wasm-lowering-and-layout.md) §6.1.4):

- The allocator is linked if at least one call of `__sai_alloc`, `__sai_realloc` or `__sai_free` remains in the item table, or `cabi_realloc` is exported. Its helpers follow by ordinary reachability.
- A program whose reachable set ([08](../spec/08-effects-and-capabilities.md) §8.12.3) contains no allocator edge never links an allocator. DCE can only remove more: an allocation that optimization proves dead does not keep the allocator (D-WASM-05). The capability check is not affected, because it uses reachability, not DCE (spec/08 §8.12.3).
- The panic reporter is linked in `Report` mode ([06](06-wasm-lowering-and-layout.md) §6.8.12.1) if at least one call of `__sai_panic` or `__sai_stack_overflow` remains. In `TrapOnly` mode it is never linked, and no panic site, message text or report buffer exists ([10](../spec/10-errors-and-panics.md) §10.5.3).
- The in-module handle table is linked if at least one call of a `__sai_handle_` function remains. Only the provision glue of a build with a hybrid dependency in source form calls them ([07](07-component-lowering.md) §7.11.10), on every target.
- Runtime state items and globals are present exactly when a linked runtime function refers to them.

### 10.1.5 Notation

The algorithms of this chapter are written in pseudocode with the following meaning. Each one is the specification of a runtime body. An implementation MAY produce different MIR for it, but the observable results (returned addresses, memory contents, calls of the channel, traps) MUST be the same for every sequence of calls, because they determine the program's memory use and its reports (P8).

- `PS` is the pointer size: 4 on wasm32 targets, 8 on wasm64 targets. `ptr` values are unsigned.
- `load(a)` and `store(a, v)` access `PS` bytes at address `a` (`rt.load`, `rt.store`). `load32`, `store32` and `store8` access 4 bytes and 1 byte.
- `align_up(x, a) = (x + a - 1) & ~(a - 1)` for a power of two `a`. Every use is preceded by a comparison that excludes overflow.
- `heap_top` stands for the global `__sai_heap_top`. `S(item)` is the address of a runtime-state item.
- `trap` is `rt.trap()`. `oom()` is the out-of-memory path of §10.2.6. It does not return.
- `ADDR_LIMIT` is `0xFFFF_0000` on wasm32 and `2^48` on wasm64 (§10.2.6).

---

## 10.2 The global allocator

### 10.2.1 Selection

Every build that links an allocator links exactly one (N-F-26):

| Condition | Allocator |
|---|---|
| The root `sai.package` declares `allocator <pkg>` ([01-package-contract.md](../package/01-package-contract.md) §1.15) | the allocator package, in every profile (§10.2.7) |
| otherwise, profile `tiny` | the tiny allocator (§10.2.5) |
| otherwise (`dev`, `release`) | the default allocator (§10.2.4) |

The allocator package MUST be a requirement of the root (`ALLOCATOR_NOT_REQUIRED`, [01-package-contract.md](../package/01-package-contract.md) §1.15). The choice depends only on the root `sai.package` and the profile. An explicit `allocator` declaration overrides the profile's default, because it is the program's explicit choice (P1). Every allocator implements the same API (§10.2.2), so the choice never changes what a program means: it changes only which addresses are returned and how much memory is used, which are unspecified ([01-introduction.md](../spec/01-introduction.md) §1.5.3, D-TGT-02).

### 10.2.2 API

Generated code, library code and glue reach the allocator only through three runtime functions. The allocation primitives `allocate`, `deallocate` and `reallocate` of `alloc.heap` ([16-core-library.md](../spec/16-core-library.md) §16.15.1) are intrinsics that impl/04 lowers to calls of them, passing the address of the `PtrMut<Byte>` and the `USize` values unchanged; impl/07 glue calls them directly ([07](07-component-lowering.md) §7.5.1, §7.6).

| Symbol | Signature (wasm32; `i64` for every parameter and result on wasm64) | Lowering of | Result |
|---|---|---|---|
| `__sai_alloc` | `(size: i32, align: i32) -> i32` | `allocate(size, align)` | the address of a new block of at least `size` bytes, aligned to `align` |
| `__sai_realloc` | `(p: i32, old_size: i32, align: i32, new_size: i32) -> i32` | `reallocate(p, old_size, align, new_size)` | the address of a block of at least `new_size` bytes, aligned to `align`, whose first `min(old_size, new_size)` bytes equal those of the old block; the old block is freed unless the result is `p` |
| `__sai_free` | `(p: i32, size: i32, align: i32) -> ()` | `deallocate(p, size, align)` | the block is freed |

**Caller contract.** For `__sai_free(p, size, align)`, and for `__sai_realloc(p, old_size, align, new_size)`, `p` MUST have been returned by `__sai_alloc` or `__sai_realloc` with exactly this size and alignment and MUST NOT have been freed since. This is the *Safety* section of [16](../spec/16-core-library.md) §16.15.1 item 4, and violating it is undefined behavior ([12](../spec/12-unsafe-and-ffi.md) §12.6.1 item 5). Safe code cannot violate it: `Box`, `Vec`, `String`, `Bytes` and arenas always free with the size and alignment they allocated with, and impl/07 frees boundary buffers in the same way. The precondition `align.is_power_of_two()` of `allocate` is a contract that is checked before the intrinsic is reached ([16](../spec/16-core-library.md) §16.15.1, panic kind `Contract`).

**Allocator guarantees** (N-F-25), for every allocator of §10.2.1:

1. A result for `size > 0` is a multiple of `align`, is at least `__heap_base`, and the block `[result, result + size)` lies inside linear memory and does not overlap any live block, static data, runtime state or the shadow stack.
2. A result for `size = 0` is `align` itself, and nothing is allocated (§10.2.3).
3. The functions never return 0 and never return to the caller after a failure. A request that cannot be satisfied takes the out-of-memory path (§10.2.6).
4. The functions never call program code and never call into the host. They use no imports.
5. The results are a deterministic function of the sequence of calls made since instantiation (P8). They do not depend on the profile except through the choice of allocator (§10.2.1).

### 10.2.3 Arguments: zero sizes, size limits and alignments

The three functions apply the following rules before the algorithm of the selected allocator. Every allocator therefore sees only non-zero sizes, and the built-in allocators see only alignments of at most 16.

```
ISIZE_MAX := 2^31 - 1 on wasm32, 2^63 - 1 on wasm64

check_align(align):
    if align == 0 or (align & (align - 1)) != 0: trap

too_large(size, align):                       // §16.15.1 item 3: align_up(size, align) > ISIZE_MAX
    return align - 1 > ISIZE_MAX or size > ISIZE_MAX - (align - 1)

__sai_alloc(size, align):
    check_align(align)
    if size == 0: return align                 // N-F-6: the address of a ZST is align(T)
    if too_large(size, align): oom()
    return alloc_impl(size, align)

__sai_free(p, size, align):
    check_align(align)
    if size == 0: return                       // p is align; nothing was allocated
    free_impl(p, size, align)

__sai_realloc(p, old_size, align, new_size):
    check_align(align)
    if old_size == 0: return __sai_alloc(new_size, align)     // p is align; it is not freed
    if new_size == 0: free_impl(p, old_size, align); return align
    if too_large(new_size, align): oom()
    return realloc_impl(p, old_size, align, new_size)
```

`alloc_impl`, `free_impl` and `realloc_impl` are those of the selected allocator: §10.2.7 for an allocator package, and for the built-in allocators:

```
alloc_impl(size, align):
    if align <= 16: return block_alloc(size)                  // §10.2.4 or §10.2.5
    return over_alloc(size, align)

free_impl(p, size, align):
    if align <= 16: block_free(p, size); return
    block_free(load(p - PS), size + align)

realloc_impl(p, old_size, align, new_size):
    if align <= 16: return block_realloc(p, old_size, new_size)
    q := over_alloc(new_size, align)
    rt.copy(q, p, min(old_size, new_size))
    block_free(load(p - PS), old_size + align)
    return q

over_alloc(size, align):                      // align >= 32
    b := block_alloc(size + align)            // b is a multiple of 16
    p := align_up(b + 1, align)               // the first multiple of align above b
    store(p - PS, b)                          // p - b is a multiple of 16, so p - PS >= b
    return p
```

- An alignment that is not a power of two traps, in every profile and with every allocator (N-F-25). `allocate` cannot pass one, because of its contract. `deallocate` and `reallocate` can only pass one through a violation of their *Safety* section, and glue never does. The check costs a few instructions and turns that undefined behavior into a trap.
- A zero size never allocates and never frees. `Vec.new()`, an empty `String` and a `Box` of a zero-sized type therefore make no allocator call in effect, and they have the pointer value `align(T)` that [06](06-wasm-lowering-and-layout.md) §6.4.3 requires (N-F-6, [16](../spec/16-core-library.md) §16.15.1 item 2).
- A size that cannot be represented after rounding takes the out-of-memory path ([16](../spec/16-core-library.md) §16.15.1 item 3, [10](../spec/10-errors-and-panics.md) §10.4.3). Because of this check, `size + align` in `over_alloc` and every size computation of §10.2.4 and §10.2.5 fits in a `USize`.
- The built-in allocators return 16-byte aligned blocks for every request, so every alignment up to 16 is satisfied without being passed on. The largest alignment of a SAI type is 8 ([06](06-wasm-lowering-and-layout.md) §6.4), so `over_alloc` is reached only by unsafe code that calls `allocate` with a larger alignment. It spends `align` extra bytes and stores the address of the underlying block in the `PS` bytes before the result. An allocator package receives every alignment unchanged and MUST honor it (§10.2.7).

### 10.2.4 The default allocator

The default allocator is a segregated free-list allocator with eight size classes and one list of large blocks, built on `memory.grow` (D-WASM-05, N-F-27). It is the allocator of the `dev` and `release` profiles. It favors simple, predictable code over low fragmentation: small blocks are never split or merged, and only large blocks are coalesced.

**Size classes.** A request of `n` bytes with `1 <= n <= 2048` uses class `c = class_of(n)`, whose blocks have `16 << c` bytes. A request with `n > 2048` is **large** and uses a block of `align_up(n, 16)` bytes.

| Class | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
|---|---|---|---|---|---|---|---|---|
| Block size | 16 | 32 | 64 | 128 | 256 | 512 | 1024 | 2048 |
| Requests | 1..16 | 17..32 | 33..64 | 65..128 | 129..256 | 257..512 | 513..1024 | 1025..2048 |

**State.** `__sai_alloc_heads` holds `head[0..7]` at offsets `c · PS` and the large-list head `L` at offset `8 · PS`. A free small block stores the address of the next free block of its class at offset 0. A free large block stores the address of the next free large block at offset 0 and its size at offset `PS`. A large block is at least `LARGE_MIN = 2064` bytes, so both fields always fit. Live blocks have no header: the size of a block is computed from the size argument of `__sai_free` and `__sai_realloc`, which the caller contract of §10.2.2 makes exact.

```
BITS      := 32 on wasm32, 64 on wasm64
SMALL_MAX := 2048
LARGE_MIN := 2064                                   // the smallest large block
H         := S(__sai_alloc_heads)
L         := H + 8 · PS                             // the address of the large-list head

class_of(n):                                        // 1 <= n <= 2048
    if n <= 16: return 0
    return BITS - rt.clz(n - 1) - 4

block_size(n):
    if n <= SMALL_MAX: return 16 << class_of(n)
    return align_up(n, 16)

push(h, p):                                         // h is the address of a list head
    store(p, load(h)); store(h, p)

block_alloc(n):
    if n <= SMALL_MAX:
        c := class_of(n); h := H + c · PS
        p := load(h)
        if p != 0: store(h, load(p)); return p
        return carve(16 << c)
    return large_alloc(align_up(n, 16))

large_alloc(b):                                     // b >= LARGE_MIN, b % 16 == 0
    slot := L; cur := load(L)
    while cur != 0:
        s := load(cur + PS)
        if s >= b:
            r := s - b
            if r >= LARGE_MIN:                      // split: the result is the end of the block
                store(cur + PS, r)
                return cur + r
            store(slot, load(cur))                  // take the whole block
            release_small_tail(cur + b, r)
            return cur
        slot := cur; cur := load(cur)
    return carve(b)

release_small_tail(a, r):                           // r % 16 == 0, r < LARGE_MIN
    c := 7
    while r > 0:
        while (16 << c) > r: c := c - 1
        push(H + c · PS, a)
        a := a + (16 << c); r := r - (16 << c)

block_free(p, n):
    if n <= SMALL_MAX: push(H + class_of(n) · PS, p); return
    large_free(p, align_up(n, 16))

large_free(p, b):                                   // b >= LARGE_MIN
    pslot := 0; prev := 0; slot := L; cur := load(L)
    while cur != 0 and cur < p:
        pslot := slot; prev := cur; slot := cur; cur := load(cur)
    if p + b == heap_top:                           // return the block to the top; cur == 0 here
        if prev != 0 and prev + load(prev + PS) == p:
            store(pslot, 0); heap_top := prev       // the last free block now ends at the top
        else:
            heap_top := p
        return
    nxt := cur
    if cur != 0 and p + b == cur:                   // merge with the following block
        b := b + load(cur + PS); nxt := load(cur)
    if prev != 0 and prev + load(prev + PS) == p:   // merge with the preceding block
        store(prev + PS, load(prev + PS) + b); store(prev, nxt)
    else:
        store(p, nxt); store(p + PS, b); store(slot, p)

block_realloc(p, old, new):
    ob := block_size(old); nb := block_size(new)
    if ob == nb: return p
    if old > SMALL_MAX and new > SMALL_MAX:
        if nb < ob:                                 // shrink in place
            if p + ob == heap_top: heap_top := p + nb
            else if ob - nb >= LARGE_MIN: large_free(p + nb, ob - nb)
            else: release_small_tail(p + nb, ob - nb)
            return p
        if p + ob == heap_top:                      // grow in place at the top
            carve(nb - ob)
            return p
    q := block_alloc(new)
    rt.copy(q, p, min(old, new))
    block_free(p, old)
    return q
```

**Invariants.** After every call that returns:

1. Every block, free or live, starts at a multiple of 16 that is at least `__heap_base`, and ends at most at `heap_top`. `heap_top` is a multiple of 16 and at most the end of linear memory.
2. No two blocks overlap, and every free block is on exactly one list.
3. The blocks of `head[c]` have exactly `16 << c` bytes.
4. The large list is sorted by increasing address, its blocks are at least `LARGE_MIN` bytes, no two of its blocks are adjacent, and none of them ends at `heap_top`.

Small blocks never return to the top and never merge. A large block taken from the large list leaves a remainder smaller than `LARGE_MIN` as small blocks of the largest classes that fit. This bounds the work of every call by the length of the large list and keeps the allocator small (§10.10).

### 10.2.5 The tiny allocator

The tiny allocator is the allocator of the `tiny` profile: a bump allocator with one free list (D-WASM-05, N-F-28). It has the smallest code of the three allocators, and its fragmentation is the highest.

**State.** `__sai_tiny_head` holds the head `T` of one free list. Every block has `align_up(n, 16)` bytes for a request of `n` bytes, so the smallest block has 16 bytes. A free block stores the next free block at offset 0 and its size at offset `PS`, which fit in 16 bytes on both pointer widths.

```
T := S(__sai_tiny_head)

block_alloc(n):
    b := align_up(n, 16)
    slot := T; cur := load(T)
    while cur != 0:                                 // first fit, in list order
        s := load(cur + PS)
        if s == b: store(slot, load(cur)); return cur
        if s > b: store(cur + PS, s - b); return cur + (s - b)    // split: the end of the block
        slot := cur; cur := load(cur)
    return carve(b)

block_free(p, n):
    b := align_up(n, 16)
    if p + b == heap_top: heap_top := p; return
    store(p, load(T)); store(p + PS, b); store(T, p)

block_realloc(p, old, new):
    ob := align_up(old, 16); nb := align_up(new, 16)
    if ob == nb: return p
    if p + ob == heap_top:
        if nb < ob: heap_top := p + nb
        else: carve(nb - ob)
        return p
    if nb < ob: block_free(p + nb, ob - nb); return p
    q := block_alloc(new)
    rt.copy(q, p, old)
    block_free(p, old)
    return q
```

The list is in LIFO order and is never sorted or merged. Only a block that ends exactly at `heap_top` gives memory back to the top. The invariants are items 1 and 2 of §10.2.4, and every block on the list is a multiple of 16 bytes.

### 10.2.6 Growing memory and running out of memory

Both built-in allocators and the region grants of an allocator package (§10.2.7) obtain fresh memory from one function, `carve`. It is the only code that executes `memory.grow` apart from the panic reporter (§10.5.5, [06](06-wasm-lowering-and-layout.md) §6.3.8), and it grows memory by exactly the number of pages needed (N-F-29).

```
ADDR_LIMIT := 0xFFFF_0000 on wasm32, 2^48 on wasm64
FAILED     := 2^32 - 1 on wasm32, 2^64 - 1 on wasm64     // the result of a failed memory.grow

carve(b):                                         // b > 0, b % 16 == 0
    p := heap_top
    if b > ADDR_LIMIT - p: oom()                  // p <= ADDR_LIMIT always holds
    end := p + b
    need := (end + 65535) >> 16                   // pages needed to cover [0, end)
    have := rt.memory_size()
    if need > have:
        if rt.memory_grow(need - have) == FAILED: oom()
    heap_top := end
    return p
```

- `ADDR_LIMIT` keeps every block end and every page count representable in a `USize`: on wasm32 the heap never uses the last 64 KiB page of the 4 GiB address space, and on wasm64 it never exceeds 256 TiB. Together with the size check of §10.2.3, it guarantees that no address computation of the allocators wraps around.
- Memory is never shrunk. `heap_top` can decrease (§10.2.4, §10.2.5), but the pages above it stay part of linear memory and are reused by later calls of `carve` without growing.
- The module declares no maximum memory size ([06](06-wasm-lowering-and-layout.md) §6.3.1), so the limit is the engine's. A host can also grow an exported memory. `carve` uses `memory.size` and is therefore correct when pages were added by someone else.
- No state is changed before the checks of `carve`, so a failed request leaves the allocator consistent. This does not matter for the program, because a failed request ends the instance.

**Out of memory.** `oom()` is a call of the runtime function `__sai_oom`, which panics with kind `OutOfMemory` and the message `out of memory`, without a location ([10](../spec/10-errors-and-panics.md) §10.4.3):

```
__sai_oom():                                      // never returns
    if the panic mode is Report: __sai_panic(rt.addr(__sai_oom$site0), 0, 0)
    trap
```

`__sai_oom$site0` is a `PanicSite` with no location and the fixed text `out of memory` (§10.5.2). In `TrapOnly` mode the function is `unreachable` alone, and impl/09 inlines it. The panic report needs no allocation (§10.5.5), so out-of-memory is reported like every other panic ([10](../spec/10-errors-and-panics.md) §10.5.2 item 3).

### 10.2.7 Allocator packages

A root package that declares `allocator <pkg>` ([01-package-contract.md](../package/01-package-contract.md) §1.15) replaces the built-in allocator with functions of `<pkg>`. This section is the contract that such a package implements (N-F-30). The runtime keeps the rules of §10.2.3 and the growth of memory, and the package manages the memory it is given.

**Interface.** The root module of `<pkg>` MUST declare the following five items, with exactly these names, kinds and signatures. Parameter names are free.

| Item | Declaration |
|---|---|
| state size | `pub const STATE_SIZE: USize`, a value in `0..=4096` |
| add a region | `pub unsafe fn add_region(state: PtrMut<Byte>, start: PtrMut<Byte>, len: USize)` |
| allocate | `pub unsafe fn allocate(state: PtrMut<Byte>, size: USize, align: USize) -> Option<PtrMut<Byte>>` |
| free | `pub unsafe fn deallocate(state: PtrMut<Byte>, ptr: PtrMut<Byte>, size: USize, align: USize)` |
| reallocate | `pub unsafe fn reallocate(state: PtrMut<Byte>, ptr: PtrMut<Byte>, old_size: USize, align: USize, new_size: USize) -> Option<PtrMut<Byte>>` |

1. The four functions are not generic, not `async`, have no `requires` or `ensures` clauses, and have an empty effect set: their declared effect sets are empty and so are their inferred `Own` sets ([08](../spec/08-effects-and-capabilities.md) §8.4). In particular they have neither `alloc` nor any capability effect.
2. None of the four functions can reach an allocation primitive of `alloc.heap` ([16](../spec/16-core-library.md) §16.15.1) through the reachability edges of [08](../spec/08-effects-and-capabilities.md) §8.12.3, including through drop glue: the set is `reach_from` of [impl/08](08-effect-and-capability-analysis.md) §8.7.1 with one `Fn` root per function. `deallocate` has no effect (D-OWN-05), so rule 1 alone would not exclude it. A reachable primitive is `ALLOCATOR_REENTRANCY` at the `allocator` declaration, with a secondary label at the call.
3. The package is used in source form in every build, because its functions are linked into the module. It MUST be a source package or a hybrid package, and an `overrides { <pkg> use artifact }` of the root does not apply to it ([04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.5.1).

A violation of item 1, of the interface table, or of item 3 is `ALLOCATOR_INTERFACE_MISMATCH`, reported at the `allocator` declaration with a secondary label at the offending item or at the missing name. The checks run in every build of the root, even when the allocator is not linked (§10.1.4), so that whether a build succeeds does not depend on optimization (P8). A dependency's `allocator` declaration is ignored and never checked (N-G-16).

**Calls.** The runtime calls the functions as follows. Each call is a runtime body (§10.1.2) that calls a function of `<pkg>`, so the package's functions are the only program code that the runtime ever calls (§10.1.2 rule 2).

```
STATE := rt.addr(__sai_custom_state) if STATE_SIZE > 0, otherwise 1

alloc_impl(size, align):                          // size > 0, align a power of two
    r := pkg.allocate(STATE, size, align)
    if r is None:
        grant(size + align + 65536)
        r := pkg.allocate(STATE, size, align)
        if r is None: oom()
    return r

free_impl(p, size, align):
    pkg.deallocate(STATE, p, size, align)

realloc_impl(p, old_size, align, new_size):       // old_size > 0, new_size > 0
    r := pkg.reallocate(STATE, p, old_size, align, new_size)
    if r is None:
        grant(new_size + align + 65536)
        r := pkg.reallocate(STATE, p, old_size, align, new_size)
        if r is None: oom()
    return r

grant(need):                                      // need > 0
    start := heap_top
    carve(align_up(need, 16))                     // grows memory only if needed; oom() if it cannot
    end := min(rt.memory_size(), ADDR_LIMIT >> 16) << 16
    heap_top := end
    pkg.add_region(STATE, start, end - start)
```

A grant hands the package everything from `heap_top` to the end of the last whole page of linear memory, capped at `ADDR_LIMIT`, and at least `need` bytes. The cap keeps `end` representable when a host has grown a wasm32 memory to 65536 pages.

**Guarantees to the package.**

1. The state is `STATE_SIZE` bytes at an address that is a multiple of 8. It is zero before the first call, and only the package writes it. When `STATE_SIZE` is 0, the state pointer is the dangling address 1 and MUST NOT be accessed.
2. `allocate` and `reallocate` are called only with `size > 0` and `new_size > 0`, with an alignment that is a power of two, and with a size that passes the check of §10.2.3. `deallocate` and `reallocate` are called only with a block that the package returned for exactly this size and alignment and that has not been freed (§10.2.2).
3. The first call of `allocate` or `reallocate` happens before any region is added, so a package that returns `None` while it has no memory receives its first region then. Each region `[start, start + len)` passed to `add_region` starts at a multiple of 16, is at least `need` bytes long, lies inside linear memory, and overlaps no static data, runtime state, shadow stack or earlier region. Regions are added at increasing addresses, and a region starts exactly where the previous one ended, so a package MAY treat consecutive regions as one.
4. The package's functions are never called during a call of one of them, and never after a trap.

**Obligations of the package.** A result `Some(p)` of `allocate` MUST point to `size` bytes, aligned to `align`, inside regions the package was given, that overlap no live block. A result of `reallocate` MUST also preserve the first `min(old_size, new_size)` bytes and free the old block unless it returns the same address. A result `None` from `reallocate` MUST leave the old block live and unchanged. A package that breaks an obligation causes undefined behavior ([12](../spec/12-unsafe-and-ffi.md) §12.6.1 item 11), which is why the functions are `unsafe` and need a `# Safety` section (`MISSING_SAFETY_DOC`, [12](../spec/12-unsafe-and-ffi.md) §12.2.2 item 4).

The retry after a grant is the only point at which the package's choice of memory matters. A package that returns `None` once more is out of memory, and the runtime takes the out-of-memory path. The extra 64 KiB of every grant avoids one grant per allocation for small requests.

### 10.2.8 Example: a bump allocator package

The package `acme:bump-alloc` never reuses memory. Its root module is:

```sai
use sai:core/ptr.{PtrMut}

type State {
    cursor: USize
    limit: USize
}

pub const STATE_SIZE: USize = 16

fn state(p: PtrMut<Byte>) -> PtrMut<State> {
    p.cast<State>()
}

/// Adds a region of free memory.
///
/// # Safety
/// `state_ptr` must point to the allocator state, and `[start, start + len)` must be unused memory.
pub unsafe fn add_region(state_ptr: PtrMut<Byte>, start: PtrMut<Byte>, len: USize) {
    let s = state(state_ptr)
    if (*s).limit != start.addr() {
        (*s).cursor = start.addr()
    }
    (*s).limit = start.addr() + len
}

/// Returns `size` bytes aligned to `align`, or `None` if the regions are exhausted.
///
/// # Safety
/// `state_ptr` must point to the allocator state, and `align` must be a power of two.
pub unsafe fn allocate(state_ptr: PtrMut<Byte>, size: USize, align: USize) -> Option<PtrMut<Byte>> {
    let s = state(state_ptr)
    let cursor = (*s).cursor
    let limit = (*s).limit
    let mask = align - 1
    if mask > limit or cursor > limit - mask {
        return None
    }
    let start = (cursor + mask) / align * align
    if size > limit - start {
        return None
    }
    (*s).cursor = start + size
    PtrMut.from_addr(start)
}

/// Frees nothing: a bump allocator never reuses memory.
///
/// # Safety
/// No requirement.
pub unsafe fn deallocate(
    _state_ptr: PtrMut<Byte>,
    _ptr: PtrMut<Byte>,
    _size: USize,
    _align: USize,
) {}

/// Moves the block to a fresh allocation.
///
/// # Safety
/// `ptr` must be a live block of `old_size` bytes returned by `allocate` with `align`.
pub unsafe fn reallocate(
    state_ptr: PtrMut<Byte>,
    ptr: PtrMut<Byte>,
    old_size: USize,
    align: USize,
    new_size: USize,
) -> Option<PtrMut<Byte>> {
    let fresh = allocate(state_ptr, new_size, align)?
    var i: USize = 0
    while i < old_size and i < new_size {
        fresh.add(i).write(*ptr.add(i))
        i += 1
    }
    Some(fresh)
}
```

The root of a program that uses it declares:

```sai-package
requires {
    acme:bump-alloc @ ^1.0
        from git "https://github.com/acme/bump-alloc"
}

allocator acme:bump-alloc
```

- `STATE_SIZE` is 16, the size of `State` on wasm64. On wasm32 `State` needs only 8 bytes, and the other 8 bytes are unused.
- Before the first region, `cursor` and `limit` are 0, so `allocate` returns `None` for every alignment greater than 1 through the first check and for alignment 1 through the second. The runtime then grants the first region, and `add_region` sets `cursor` to its start. Later regions start at the old `limit`, so `add_region` only extends `limit`.
- The arithmetic cannot overflow, which matters because every integer operation of the package is checked ([10](../spec/10-errors-and-panics.md) §10.4.2): `or` evaluates `limit - mask` only when `mask <= limit`, `cursor + mask` is then at most `limit`, and `limit` is at most `ADDR_LIMIT` (§10.2.6). A panic in an allocator package is reported like any other, because the report needs no allocation (§10.5.5).
- `write` is used instead of assignment because the fresh bytes hold no value yet ([12](../spec/12-unsafe-and-ffi.md) §12.4.4).

---

## 10.3 `cabi_realloc` and the in-module handle table

### 10.3.1 Definition

`cabi_realloc` is the Canonical ABI `realloc` function of a component build. impl/07 decides when it is exported and which glue and host calls use it ([07](07-component-lowering.md) §7.5.1, N-D-49). This section defines its body (N-F-31). It exists only on wasm32, because component builds for `wasm64-wasi` are not implemented in v0.1 ([targets-and-profiles.md](../targets/targets-and-profiles.md) §2).

```
cabi_realloc(old_ptr, old_size, align, new_size):
    if align not in {1, 2, 4, 8}: trap
    return __sai_realloc(old_ptr, old_size, align, new_size)
```

By §10.2.3 this gives exactly the table of [07](07-component-lowering.md) §7.5.1:

| Arguments | Path in §10.2.3 | Result |
|---|---|---|
| `old_size = 0`, `new_size = 0` | `__sai_alloc(0, align)` | `align`; nothing is allocated |
| `old_size = 0`, `new_size > 0` | `__sai_alloc(new_size, align)` | a fresh block; `old_ptr` is ignored |
| `old_size > 0`, `new_size = 0` | `free_impl(old_ptr, old_size, align)` | `align`; the block is freed |
| `old_size > 0`, `new_size > 0` | `realloc_impl` | the reallocated block |

- A block returned for a non-zero size is at least `__heap_base`, which is at least 8 ([06](06-wasm-lowering-and-layout.md) §6.3.2), so it is never below 8. A zero-size result is `align` itself, which can be 1, 2 or 4. The Canonical ABI never dereferences the pointer of a zero-length buffer, and impl/07 treats such a buffer as empty without looking at its address.
- A block obtained from `cabi_realloc` is an ordinary allocation. Glue and library code free it with `__sai_free` and the same size and alignment, and a lifted `String`, `Vec<T>` or `Bytes` takes ownership of it without copying ([07](07-component-lowering.md) §7.4.3).
- `cabi_realloc` uses the selected allocator (§10.2.1), including an allocator package. It runs no other code and calls no import, so it is never reentered.
- A failed allocation takes the out-of-memory path (§10.2.6). In `Report` mode the report is written to standard error if the component imports it ([10](../spec/10-errors-and-panics.md) §10.5.3), even when the host is in the middle of lowering arguments. The instance then traps, which the host observes as a trap of the export call.

### 10.3.2 Lowering

On wasm32, after inlining of the alignment check of `__sai_realloc` (which `cabi_realloc` has already made redundant, [09](09-optimization-and-size.md) §9.4):

```wat
(func $cabi_realloc (export "cabi_realloc")
    (param $old_ptr i32) (param $old_size i32) (param $align i32) (param $new_size i32)
    (result i32)
  ;; align must be 1, 2, 4 or 8
  (if (i32.or
        (i32.ne (i32.popcnt (local.get $align)) (i32.const 1))
        (i32.gt_u (local.get $align) (i32.const 8)))
    (then (unreachable)))
  (if (i32.eqz (local.get $old_size))
    (then (return (call $__sai_alloc (local.get $new_size) (local.get $align)))))
  (if (i32.eqz (local.get $new_size))
    (then
      (call $__sai_free_impl (local.get $old_ptr) (local.get $old_size) (local.get $align))
      (return (local.get $align))))
  (call $__sai_realloc_impl
    (local.get $old_ptr) (local.get $old_size) (local.get $align) (local.get $new_size)))
```

`$__sai_free_impl` and `$__sai_realloc_impl` are the runtime functions that implement `free_impl` and `realloc_impl` of §10.2.3. Whether they are separate functions or are inlined is decided by [09](09-optimization-and-size.md) §9.4. Their names are `RuntimeSymbol`s ([06](06-wasm-lowering-and-layout.md) §6.10.2) that appear only in the name section and in `sai size`.

### 10.3.3 The in-module handle table

The handles of the resources that a hybrid dependency provides within the build are indices into one table in linear memory, which the runtime manages ([07](07-component-lowering.md) §7.6.5 and §7.11.10, N-D-108). It replaces the Component Model's resource table for these resources only, and the instance's resource table keeps every other handle. The table is the same on every target, because core modules have no resource table (N-F-95).

**Layout.** The runtime-state item `__sai_handles` (§10.1.3 item 6) is the header of the table. It has four fields of `PS` bytes:

| Offset | Field | Content |
|---|---|---|
| 0 | `base` | the address of the entry array; 0 before the first handle is created |
| `PS` | `cap` | the number of entries that the array can hold |
| `2 · PS` | `len` | the number of entries ever used; the valid indices are `1..=len` |
| `3 · PS` | `free` | the most recently freed index, or 0 when no entry is free |

An entry has the size `E = PS + 8` and the alignment `PS`. The entry of index `h` is at `entry(h) = base + (h - 1) · E`:

| Offset | Field | Content |
|---|---|---|
| 0 | `rep` | the rep of a live handle; for a free entry, the next free index, or 0 at the end of the free list |
| `PS` | `kind` (4 bytes) | the type index `k ≥ 1` of the resource of a live handle ([07](07-component-lowering.md) §7.11.10 item 3); 0 for a free entry |
| `PS + 4` | `lends` (4 bytes) | the number of calls to which a live handle is currently lent |

A handle is an `i32` on every target, as in the Canonical ABI. `T` is `S(__sai_handles)`, and `MAX_HANDLES` is `2^28 - 1`, the largest index of a Component Model table.

**Operations.** `live(k, h)` is part of every operation that uses it. The algorithms are:

```text
__sai_handle_new(k, rep) -> h:
    h := load(T + 3·PS)
    if h != 0:
        e := entry(h)
        store(T + 3·PS, load(e))                   // pop the free list: last freed, first reused
    else:
        n := load(T + 2·PS)
        if n == MAX_HANDLES: trap                  // the table is full
        if n == load(T + PS): __sai_handle_grow()
        h := n + 1
        store(T + 2·PS, h)
        e := entry(h)
    store(e, rep); store32(e + PS, k); store32(e + PS + 4, 0)
    return h

__sai_handle_grow():
    cap := load(T + PS)
    new := min(max(4, 2 · cap), MAX_HANDLES)
    store(T, __sai_realloc(load(T), cap · E, PS, new · E))   // a fresh block when cap = 0
    store(T + PS, new)

live(k, h) -> e:
    if h == 0 or h > load(T + 2·PS): trap          // never created
    e := entry(h)
    if load32(e + PS) != k: trap                   // free (kind 0) or of another resource type
    return e

__sai_handle_rep(k, h) -> rep:
    return load(live(k, h))

__sai_handle_lend(k, h) -> rep:
    e := live(k, h)
    n := load32(e + PS + 4)
    if n == 0xFFFF_FFFF: trap
    store32(e + PS + 4, n + 1)
    return load(e)

__sai_handle_unlend(h):
    e := entry(h)
    store32(e + PS + 4, load32(e + PS + 4) - 1)

__sai_handle_move(k, h):
    e := live(k, h)
    if load32(e + PS + 4) != 0: trap               // an owned handle cannot move while it is lent

__sai_handle_drop(k, h) -> rep:
    e := live(k, h)
    if load32(e + PS + 4) != 0: trap               // an owned handle cannot be dropped while it is lent
    rep := load(e)
    store(e, load(T + 3·PS)); store32(e + PS, 0)
    store(T + 3·PS, h)
    return rep
```

- The checks are those of the Component Model's `resource.rep`, `resource.drop` and the lifting of `own` and `borrow` handles: an index that is not live, a handle of another resource type, an owned handle that is moved or dropped while it is lent, and a full table. The callers in impl/07 rely on them and check nothing themselves (N-D-108).
- `__sai_handle_unlend` checks nothing. It is called only by provision glue, for a handle that the same call has lent, and a lent handle can be neither moved nor dropped, so its entry is still live.
- Index 0 is never a handle, and freed indices are reused last freed first, as in the Component Model. The handles of a program are therefore a deterministic function of its sequence of calls (P8). Because the in-module table and the instance's resource table never exchange handles, the same number can denote different handles in the two tables ([07](07-component-lowering.md) §7.6.5).
- `__sai_handle_drop` does not run the destructor. Its only caller, the drop adapter of impl/07, calls the provider's `[dtor]r` with the returned rep right afterwards ([07](07-component-lowering.md) §7.11.10).
- The products `(h - 1) · E` and `cap · E` cannot overflow, because `h ≤ cap ≤ MAX_HANDLES` and `MAX_HANDLES · E < 2^32` on wasm32 (rule 3 of §10.1.2). A failure of `__sai_realloc` takes the out-of-memory path (§10.2.6).
- The array is never shrunk or freed. When the instance is torn down, no destructor runs for the live handles, as for every resource ([spec/13](../spec/13-components-and-wit.md) §13.10.4).

---

## 10.4 Arenas

### 10.4.1 Division of work

An arena is the hidden `Arena` value of a `region` block ([07](../spec/07-ownership-and-borrowing.md) §7.15, [16](../spec/16-core-library.md) §16.15.5, D-OWN-07). Its methods are library code of `sai:alloc`, and it obtains its chunks from the global allocator through `allocate` and returns them through `deallocate`. The runtime has no arena function and no arena state (N-F-32). This section fixes the layout and the algorithm of the reference `sai:alloc`, because the chunk sizes are "part of the runtime" ([16](../spec/16-core-library.md) §16.15.5 item 2) and because they determine the allocator traffic of a program, which `sai size` and tests observe.

### 10.4.2 Layout

`Arena` is a plain record of three `USize` fields, so its size is `3 · PS` and its alignment is `PS` ([06](06-wasm-lowering-and-layout.md) §6.4):

| Offset | Field | Meaning |
|---|---|---|
| 0 | `chunk` | the address of the newest chunk, or 0 if the arena has none |
| `PS` | `cursor` | the first free byte of the newest chunk |
| `2 · PS` | `limit` | the end of the newest chunk |

A **chunk** is one block of the global allocator, of `size` bytes with alignment 16. It starts with a 16-byte header:

| Offset in the chunk | Field |
|---|---|
| 0 | `prev`: the address of the next older chunk, or 0 |
| `PS` | `size`: the size of the chunk in bytes, including the header |
| 16 | the first byte available for allocations |

`Arena` has interior mutability: `alloc`, `alloc_str` and `alloc_slice` take `self: borrow Self` and change `cursor`, `limit` and `chunk` while the shared loan held by `r` is live. The methods write the fields through a pointer derived from `self`. This is the arena exception of [spec/12](../spec/12-unsafe-and-ffi.md) §12.6.4 rule 5, the only type whose places are written under a shared loan. `Arena` has no type parameter, so the variance rule of [07](../spec/07-ownership-and-borrowing.md) §7.6.3 is not affected. No optimization of [09](09-optimization-and-size.md) relies on the immutability of a place behind a shared borrow ([09](09-optimization-and-size.md) §9.4.1 item 5), so the exception needs no support in the optimizer.

### 10.4.3 Algorithm

```
FIRST_CHUNK := 4096
MAX_CHUNK   := 65536
HEADER      := 16

region_new():                                     // allocates nothing (§16.15.5)
    return Arena { chunk: 0, cursor: 0, limit: 0 }

alloc_bytes(a, size, align):                      // align is a power of two, at most 8
    if size == 0: return align
    if a.chunk != 0:
        start := align_up(a.cursor, align)        // a.cursor <= a.limit, so no overflow
        if start <= a.limit and size <= a.limit - start:
            a.cursor := start + size
            return start
    return new_chunk(a, size)

new_chunk(a, size):
    if size > ISIZE_MAX - 31: out_of_memory()
    want := FIRST_CHUNK if a.chunk == 0 else min(2 · load(a.chunk + PS), MAX_CHUNK)
    need := align_up(HEADER + size, 16)
    if need > want: want := need                  // a dedicated chunk for a large request
    c := allocate(want, 16)
    store(c, a.chunk); store(c + PS, want)
    a.chunk := c
    a.cursor := c + HEADER + size
    a.limit := c + want
    return c + HEADER

drop(a):                                          // the Drop impl of Arena
    c := a.chunk
    while c != 0:
        prev := load(c); size := load(c + PS)
        deallocate(c, size, 16)
        c := prev
```

- `out_of_memory()` raises the `OutOfMemory` panic. The library does it by calling `allocate(USIZE_MAX, 16)`, which panics with that kind and message because the size is not representable ([16](../spec/16-core-library.md) §16.15.1 item 3). Without the check, `HEADER + size` would overflow and panic with `Overflow(Add)` instead, which [10](../spec/10-errors-and-panics.md) §10.4.3 does not allow for an allocation failure.
- `c + HEADER` is a multiple of 16, so the first allocation of a chunk is aligned for every SAI type. The data of a chunk starts at the same offset on both pointer widths.
- When a request does not fit, the free tail of the current chunk is abandoned and the new chunk becomes current. The tail is freed with its chunk when the arena is dropped.
- Chunk sizes grow 4 KiB, 8 KiB, 16 KiB, 32 KiB, 64 KiB, 64 KiB, and so on. A chunk is at most 64 KiB unless one request needs more, and the chunk after a dedicated chunk has 64 KiB. With the default allocator every chunk is a large block (§10.2.4), so it is coalesced and returned to the top when the arena is dropped.
- An arena that never allocates never calls the allocator. Its drop loop runs zero times.

The three methods are:

| Method | Implementation |
|---|---|
| `alloc<T: Copy>(value)` | `p := alloc_bytes(size_of<T>(), align_of<T>())`, then `PtrMut<T>` at `p` `.write(value)`, and the result is `borrow mut *p` |
| `alloc_str(s)` | `p := alloc_bytes(s.len(), 1)`, then `memory.copy` of the `s.len()` bytes, and the result is the `Str` with address `p` and length `s.len()` |
| `alloc_slice<T: Copy>(s)` | `p := alloc_bytes(s.len() · size_of<T>(), align_of<T>())`, then `memory.copy` of those bytes, and the result is the `Slice<T>` with address `p` and length `s.len()` |

The product in `alloc_slice` cannot overflow, because `s` is a valid slice, and a valid slice covers at most `ISIZE_MAX` bytes ([12](../spec/12-unsafe-and-ffi.md) §12.6.2). A zero-sized `T`, an empty `Str` and an empty `Slice` take no space and return the address `align` (N-F-6).

### 10.4.4 Example

```sai
@derive(Copy, Clone)
type Point {
    x: I32
    y: I32
}

fn sum_of_products(n: I32) -> I64
    effects { alloc }
{
    region r {
        var total: I64 = 0
        var i: I32 = 0
        while i < n {
            let p = r.alloc(Point { x: i, y: i + 1 })
            total += I64.from(p.x) * I64.from(p.y)
            i += 1
        }
        total
    }
}
```

Each `Point` takes 8 bytes with alignment 4, so points are packed without gaps. The first chunk has 4096 bytes and holds 510 points after its 16-byte header. The 511th call allocates a chunk of 8192 bytes (1022 points), and the following chunks hold 2046, 4094 and then 8190 points each. At the end of the block, the drop of the hidden arena frees the chunks, newest first, with `deallocate(c, size, 16)`. For 10,000 points the function makes 5 calls of `allocate` and 5 calls of `deallocate` instead of 10,000 of each, which is the purpose of arenas (D-OWN-07).

---

## 10.5 Panic reporting

### 10.5.1 Overview

This section implements panic reporting as [10](../spec/10-errors-and-panics.md) §10.5 and §10.6 define it. In `Report` mode, the panic sequence of [06](06-wasm-lowering-and-layout.md) §6.8.12.2 calls `__sai_panic(site, a, b)`. The function builds the report in linear memory, delivers it once through the build's channel, and traps. In `TrapOnly` mode none of this exists.

```
__sai_panic(site, a, b):                          // never returns
    if __sai_panicking != 0: trap                 // N-C-82: no second report
    __sai_panicking := 1
    len := report_len(site, a, b)                 // §10.5.5
    buf := report_buffer(len)                     // §10.5.5
    write_report(buf, site, a, b)                 // §10.5.5
    deliver(buf, len)                             // §10.5.4
    trap
```

`__sai_panic` follows every rule of §10.1.2. It has no frame, contains no check and calls no program code, so it works after the shadow stack is exhausted, inside the allocator, and when the allocator's state is inconsistent. It never calls the allocator ([10](../spec/10-errors-and-panics.md) §10.5.2 item 3).

### 10.5.2 Panic sites

A **panic site** is a record that describes one panic: its location and how its message is formed (N-F-33). The panic sequence passes its address as `site`.

| Offset (wasm32) | Offset (wasm64) | Field | Type | Meaning |
|---|---|---|---|---|
| 0 | 0 | `file_ptr` | `ptr` | address of the file string |
| 4 | 8 | `file_len` | `USize` | length of the file string; 0 means that the panic has no location |
| 8 | 16 | `text_ptr` | `ptr` | address of the fixed message text |
| 12 | 24 | `text_len` | `USize` | length of the fixed message text |
| 16 | 32 | `line` | `u32` | 1-based line |
| 20 | 36 | `column` | `u32` | 1-based column, in UTF-8 bytes |
| 24 | 40 | `form` | `u32` | how the message is formed, see below |

The record has 28 bytes with alignment 4 on wasm32, and 48 bytes (with 4 bytes of padding at the end) with alignment 8 on wasm64. When `file_len` is 0, `file_ptr`, `line` and `column` are 0.

| `form` | Name | Message | `a`, `b` | Used for |
|---|---|---|---|---|
| 0 | `Text` | the fixed text `[text_ptr, text_ptr + text_len)` | 0, 0 | every kind whose message is fixed at compile time: `Overflow(..)`, `DivByZero`, `RemByZero`, `ShiftOverflow`, `Contract(id)`, `Invariant(id)`, `Refinement(id)`, `UserAssert` without a message operand, `unreachable`, `Unwrap` with a library message, `StackOverflow`, `OutOfMemory` |
| 1 | `Bounds` | `index out of bounds: index <a>, length <b>` | the index and the length | `BoundsCheck` |
| 2 | `Dynamic` | the bytes `[a, a + b)` | the address and the length of a `Str` | `panic(msg)`, `assert` with a message operand, and the library items whose message is an argument (`expect`, `expect_err`) |

- The fixed texts are exactly those of [10](../spec/10-errors-and-panics.md) §10.4.4 and of [09](../spec/09-contracts-and-refinements.md) §9.7.1, computed at compile time. For `Bounds` and `Dynamic`, `text_ptr` and `text_len` are 0.
- **File string.** The file string is `<namespace>:<name>@<version>/<path>`, where `<path>` is the path of the source file relative to the root of its package, with `/` as the separator ([10](../spec/10-errors-and-panics.md) §10.4.5, [01](../spec/01-introduction.md) §1.6 item 5). Example: `acme:app@0.1.0/src/main.sai`. It contains no absolute path. The version is included because two lines of one package can coexist in a build (D-PKG-02), as in symbols ([06](06-wasm-lowering-and-layout.md) §6.10.2).
- Panic sites, file strings and fixed texts are static data items ([06](06-wasm-lowering-and-layout.md) §6.3.5). `file_ptr` and `text_ptr` are relocations. Identical items are shared, so a file string exists once however many sites refer to it, and identical site records share one item ([06](06-wasm-lowering-and-layout.md) §6.8.14.3). The runtime's own sites are `__sai_oom$site0` and `__sai_stack_overflow$site0`, which have no location.
- **Run-time locations.** A panic raised in a library body takes the position of the call that entered the library ([10](../spec/10-errors-and-panics.md) §10.4.5). When that position is not a compile-time constant of the body, the code that raises the panic builds the panic site in its own frame from the location passed to it by impl/04, and passes the address of that record ([06](06-wasm-lowering-and-layout.md) §6.8.12.2, [04](04-mir.md) §4.13.7). The runtime reads a site only through the fields above, so both kinds of record are treated alike.

### 10.5.3 Channels

The panic mode and the channel are fixed for a build ([10](../spec/10-errors-and-panics.md) §10.5.3, N-C-78, [06](06-wasm-lowering-and-layout.md) §6.8.12.1):

| Artifact | Profile | Condition | Mode | Channel |
|---|---|---|---|---|
| test harness ([impl/13](13-cli.md) §13.6.6.2) | `dev`, `release` | always | `Report` | the harness channel |
| core module (`wasm32-core`, `wasm64-core`) | `dev`, `release` | the root declares a `@panic_hook` | `Report` | the hook |
| component (`wasm32-wasi`, or `sai component build`) | `dev`, `release` | `wasi:cli/stderr` is in the component's import set `Imp` ([07](07-component-lowering.md) §7.11.8) | `Report` | standard error |
| any | `tiny` | — | `TrapOnly` | none |
| any | `dev`, `release` | no channel | `TrapOnly` | none |

- A test harness is a core module of any Wasm target, and the first matching row applies to it: its channel is the harness channel of [spec/14](../spec/14-testing.md) §14.4.2 (N-F-82). The `@panic_hook` of its root is never imported, and it has no `wasi:cli/stderr` import.
- A component made by `sai component build` from a core target imports nothing ([13](../spec/13-components-and-wit.md) N-D-32), so it never has a channel. The `@panic_hook` of its root is never called ([10](../spec/10-errors-and-panics.md) §10.6.2 item 1).
- The channel never adds an import that the program does not otherwise have. The core import of the hook is present exactly when the mode is `Report` or SAI code calls the hook (N-C-80). In a component, the core imports used by the stderr channel (§10.5.4) are imports of the core module that the component already satisfies, because `wasi:cli/stderr` and `wasi:io/streams` are already in `Imp`, so the component's own imports do not change (N-D-59). These are the only imports whose presence depends on the profile ([06](06-wasm-lowering-and-layout.md) §6.10.1, N-F-20).

### 10.5.4 Delivery

```
deliver(p, len):
    channel is the hook:
        hook(p, len)                              // one call; the hook may return or trap
    channel is the harness channel:
        harness_panic(p, len)                     // one call; the runner copies the report and returns
    channel is standard error:
        h := get_stderr()
        while len > 0:
            n := min(len, 4096)
            blocking_write_and_flush(h, p, n, S(__sai_panic_ret))
            if load8(S(__sai_panic_ret)) != 0: return    // stream-error: no retry
            p := p + n; len := len - n
```

**Hook.** `hook` is the core import of the root's `@panic_hook` function, called with the address and the length of the report as `ptr` values ([10](../spec/10-errors-and-panics.md) §10.6.2 item 3). It is called exactly once per instance, because `__sai_panicking` stays 1 afterwards. The module exports its memory as `memory` ([06](06-wasm-lowering-and-layout.md) §6.3.7), and the report lies inside it, so the host can read it during the call.

**Harness** (N-F-82). `harness_panic` is `CoreImport { module: "sai-harness", field: "panic", sig: (USize, USize) -> () }` ([04](04-mir.md) §4.13.9), whose core type is `(func (param ptr ptr))`. It is called exactly once per instance, like the hook, and the runner reads the report from the exported memory during the call ([impl/13](13-cli.md) §13.6.6.3 item 3). No SAI item declares it, so it adds no effect and no capability (spec/14 §14.4.2 item 2).

**Standard error** (N-F-36). The stderr channel uses two core imports, with the names and signatures that impl/07 gives every WASI import on wasm32 ([07](07-component-lowering.md) §7.3.3, §7.10.1). The runtime body calls each as a `CoreImport` instance ([04](04-mir.md) §4.13.9), because no SAI item declares it:

| Core import (module, field) | Core type | Use |
|---|---|---|
| `wasi:cli/stderr@0.2.3`, `get-stderr` | `(func (result i32))` | returns an owned `output-stream` handle |
| `wasi:io/streams@0.2.3`, `[method]output-stream.blocking-write-and-flush` | `(func (param i32 i32 i32 i32))` | the handle, the address and the length of the bytes, the return area |

- The return area `__sai_panic_ret` receives the lowered `result<_, stream-error>`: byte 0 is the discriminant (0 for `ok`, 1 for `err`), and the error case occupies bytes 4 to 11 ([07](07-component-lowering.md) §7.3.3, §7.4.6). Only byte 0 is read.
- `blocking-write-and-flush` accepts at most 4096 bytes per call (WASI 0.2), so a long report is written in pieces of 4096 bytes. The pieces form one report: [10](../spec/10-errors-and-panics.md) §10.5.2 item 4 requires one delivery of one text, not one call.
- At the first error the runtime stops writing and traps ([10](../spec/10-errors-and-panics.md) §10.5.3: "If writing the report to standard error fails, the instance traps without retrying").
- Neither the stream handle nor the handle of an error is dropped. The instance traps immediately afterwards, and the host frees its resources ([13](../spec/13-components-and-wit.md) §13.13).
- The two imports may already exist because the program writes to standard error itself. A core import is identified by its module and field names, so the runtime and the program then use the same import ([06](06-wasm-lowering-and-layout.md) §6.10.3).

### 10.5.5 Report format and buffer

**Format** (N-F-34). The report is one line of UTF-8 text, terminated by a line feed (`0x0A`):

| Panic | Report |
|---|---|
| with a location | `panic at <file>:<line>:<column>: <message>` followed by a line feed |
| without a location | `panic: <message>` followed by a line feed |

`<file>` is the file string of §10.5.2, `<line>` and `<column>` are decimal numbers without leading zeros, and `<message>` is the message of the site's form. There is no other text: no thread name, no backtrace, no address, no hint. The report of a panic is the same in `dev` and `release` and on every target of the same pointer width, because nothing in it depends on the profile ([10](../spec/10-errors-and-panics.md) §10.5.2 item 2). The index and length of a `Bounds` message can differ between pointer widths only in their value, never in their format.

Examples:

```
panic at acme:app@0.1.0/src/main.sai:2:15: index out of bounds: index 0, length 0
panic at acme:app@0.1.0/src/main.sai:17:14: mode_from: unknown mode code
panic at acme:app@0.1.0/src/geometry.sai:41:22: precondition failed: d != 0
panic: out of memory
panic: stack overflow
```

**Length and writing.** The report is measured before it is written, so that it can be placed in one contiguous buffer:

```
FRAG := address of __sai_panic$data0            // the 45 bytes "panic at index out of bounds: index , length "

digits(x):                                      // number of decimal digits of x, at least 1
    n := 1
    while x >= 10: x := x / 10; n := n + 1
    return n

message_len(site, a, b):
    form 0: return site.text_len
    form 1: return 27 + digits(a) + 9 + digits(b)
    form 2: return b

report_len(site, a, b):
    if site.file_len == 0: head := 7                                  // "panic: "
    else: head := 9 + site.file_len + 1 + digits(site.line) + 1 + digits(site.column) + 2
    return head + message_len(site, a, b) + 1

write_report(p, site, a, b):
    if site.file_len == 0:
        p := put(p, FRAG, 5)                    // "panic"
        p := put_byte(p, ':'); p := put_byte(p, ' ')
    else:
        p := put(p, FRAG, 9)                    // "panic at "
        p := put(p, site.file_ptr, site.file_len)
        p := put_byte(p, ':'); p := put_dec(p, site.line)
        p := put_byte(p, ':'); p := put_dec(p, site.column)
        p := put_byte(p, ':'); p := put_byte(p, ' ')
    form 0: p := put(p, site.text_ptr, site.text_len)
    form 1: p := put(p, FRAG + 9, 27); p := put_dec(p, a)          // "index out of bounds: index "
            p := put(p, FRAG + 36, 9); p := put_dec(p, b)          // ", length "
    form 2: p := put(p, a, b)
    put_byte(p, 0x0A)

put(p, src, n): rt.copy(p, src, n); return p + n
put_byte(p, c): rt.store8(p, c); return p + 1
put_dec(p, x):
    k := digits(x); i := k
    while i > 0: i := i - 1; rt.store8(p + i, '0' + x % 10); x := x / 10
    return p + k
```

A site with a form other than 0, 1 or 2 cannot occur. The compiler creates every site, and the runtime treats any other value as form 0.

**Buffer** (N-F-35). A report of at most 1024 bytes is written to the runtime-state item `__sai_panic_buf`. A longer report, which is possible only with a long `Dynamic` message or a very long file string, is written above all memory in use:

```
report_buffer(len):
    if len <= 1024: return S(__sai_panic_buf)
    base := heap_top if an allocator is linked, otherwise __heap_base
    if len > ADDR_LIMIT - base: trap
    need := (base + len + 65535) >> 16; have := rt.memory_size()
    if need > have and rt.memory_grow(need - have) == FAILED: trap
    return base
```

- This keeps the rule that a message is never truncated ([10](../spec/10-errors-and-panics.md) §10.4.4) without calling the allocator ([10](../spec/10-errors-and-panics.md) §10.5.2 item 3). The memory above `heap_top` belongs to no block and to no region of an allocator package, so writing there changes nothing that is live. The instance traps right after the delivery, so the memory is never used again.
- If memory cannot grow, the instance traps without a report. This is the only case in which a `Report` build traps without reporting, and it requires a message of more than about 1000 bytes in an instance whose memory is exhausted.
- This is the second user of `memory.grow`, besides `carve` (§10.2.6, [06](06-wasm-lowering-and-layout.md) §6.3.8).
- The 1024 bytes of `__sai_panic_buf` are present in every `Report` build, whether or not a long report is possible. They are zero-initialized runtime state and cost no bytes in the module ([06](06-wasm-lowering-and-layout.md) §6.3.6).

### 10.5.6 Stack overflow

The prologue check of [06](06-wasm-lowering-and-layout.md) §6.6.6 calls `__sai_stack_overflow` in `Report` mode when a frame does not fit:

```
__sai_stack_overflow():                         // never returns
    __sai_panic(rt.addr(__sai_stack_overflow$site0), 0, 0)
    trap
```

`__sai_stack_overflow$site0` has no location, form 0 and the text `stack overflow` ([10](../spec/10-errors-and-panics.md) §10.4.3). Because no runtime function has a frame (§10.1.2 rule 1), the call works although `__sp` is too low for any frame. The engine's call stack is not exhausted at this point: the shadow stack overflowed first. If it is (a frame-less recursion that is deep enough), the engine traps on its own, which is a plain trap and not a panic ([10](../spec/10-errors-and-panics.md) §10.4.3).

In `TrapOnly` mode the prologue contains `unreachable` instead of the call, and `__sai_stack_overflow` does not exist.

### 10.5.7 Out of memory

`__sai_oom` (§10.2.6) is the only source of the `OutOfMemory` panic. It is reached from `carve`, from the size check of §10.2.3, from a second `None` of an allocator package (§10.2.7), and from arenas through `allocate` (§10.4.3). Reporting it needs no allocation: the report has no location, so it always fits in `__sai_panic_buf`. If the allocator was called while the panic reporter was already running, which cannot happen because the reporter never allocates, the reentrancy guard would trap.

### 10.5.8 Reentrancy and nesting

`__sai_panicking` implements the safeguard of [10](../spec/10-errors-and-panics.md) §10.6.2 item 5 (N-C-82). It is set before anything else and never cleared. The only ways to raise a second panic while the first is being reported are:

- the host calls an export of the instance from inside the hook, or from inside `blocking-write-and-flush`, and that call panics. The host MUST NOT do this, and the guard turns the second panic into a plain trap.
- the host calls an export after the instance trapped. The host MUST NOT do this either ([10](../spec/10-errors-and-panics.md) §10.7.2 item 3, N-C-83). If it does anyway and the call panics, the guard traps without a report, because `__sai_panicking` is still 1.

A trap during the report, for example in the host's hook, ends the instance like the trap after the report.

### 10.5.9 Lowering

A core module on wasm32 with a hook declared as `fn report_panic(ptr: USize, len: USize)` in `extern "wasm" module "env"` of the root module `host` of `acme:app`:

```wat
(import "env" "report_panic" (func $acme:app@0.1.0/host.report_panic (param i32 i32)))

(global $__sai_panicking (mut i32) (i32.const 0))

(func $__sai_panic (param $site i32) (param $a i32) (param $b i32)
  (local $len i32) (local $buf i32)
  (if (global.get $__sai_panicking) (then (unreachable)))
  (global.set $__sai_panicking (i32.const 1))
  (local.set $len (call $__sai_panic_len (local.get $site) (local.get $a) (local.get $b)))
  (local.set $buf (call $__sai_panic_buffer (local.get $len)))
  (call $__sai_panic_write (local.get $buf) (local.get $site) (local.get $a) (local.get $b))
  (call $acme:app@0.1.0/host.report_panic (local.get $buf) (local.get $len))
  (unreachable))

(func $__sai_stack_overflow
  (call $__sai_panic (i32.const 0x0010_0040) (i32.const 0) (i32.const 0))  ;; __sai_stack_overflow$site0
  (unreachable))
```

The stderr delivery loop of a component build in `Report` mode:

```wat
(import "wasi:cli/stderr@0.2.3" "get-stderr" (func $get_stderr (result i32)))
(import "wasi:io/streams@0.2.3" "[method]output-stream.blocking-write-and-flush"
  (func $blocking_write_and_flush (param i32 i32 i32 i32)))

(func $__sai_panic_deliver (param $p i32) (param $len i32)
  (local $h i32) (local $n i32)
  (local.set $h (call $get_stderr))
  (block $done
    (loop $next
      (br_if $done (i32.eqz (local.get $len)))
      (local.set $n
        (select (local.get $len) (i32.const 4096)
          (i32.lt_u (local.get $len) (i32.const 4096))))
      (call $blocking_write_and_flush
        (local.get $h) (local.get $p) (local.get $n) (i32.const 0x0010_0458))  ;; __sai_panic_ret
      (br_if $done (i32.load8_u (i32.const 0x0010_0458)))
      (local.set $p (i32.add (local.get $p) (local.get $n)))
      (local.set $len (i32.sub (local.get $len) (local.get $n)))
      (br $next))))
```

The addresses are examples. `$get_stderr` and `$blocking_write_and_flush` stand for the `ImportSymbol`s `import:wasi:cli/stderr@0.2.3#get-stderr` and `import:wasi:io/streams@0.2.3#[method]output-stream.blocking-write-and-flush` ([06](06-wasm-lowering-and-layout.md) §6.10.2), which are not valid WAT identifiers. `__sai_panic_len`, `__sai_panic_buffer`, `__sai_panic_write` and `__sai_panic_deliver` are the runtime functions of `report_len`, `report_buffer`, `write_report` and `deliver`. Whether they remain separate functions or are inlined into `__sai_panic` is decided by [09](09-optimization-and-size.md) §9.4; their behavior is the same either way.

---

## 10.6 The shadow stack

The shadow stack has no runtime function and no runtime state of its own. impl/06 owns all of it ([06](06-wasm-lowering-and-layout.md) §6.3.2, §6.3.3, §6.6.6):

- `__sp` is global 0 when the module has a stack, and starts at `__stack_high = 8 + S`, with `S = 1 MiB`.
- Each function with a frame checks `__sp < 8 + F` in its prologue, in every profile (D-WASM-02), and calls `__sai_stack_overflow` (§10.5.6) or executes `unreachable`.
- The runtime's part is that no runtime function has a frame (§10.1.2 rule 1). The allocator, the panic reporter and `cabi_realloc` therefore never touch `__sp`, and they run correctly at any stack depth, including from the overflow path itself.
- Between two calls into the instance, `__sp` equals `__stack_high`, because every function restores it before returning. A reentrant call from the host during an import ([12](../spec/12-unsafe-and-ffi.md) §12.7.2) uses the stack below the frames of the outer call.

---

## 10.7 Initialization and lifecycle

### 10.7.1 Instantiation

Instantiating a module runs no code (N-F-37):

1. The module has no start function and exports no `_start`, `_initialize` or other initialization function. The reference implementation never emits a Wasm `start` section.
2. Every piece of runtime state has its initial value without running code: static data comes from active data segments ([06](06-wasm-lowering-and-layout.md) §6.3.5), runtime-state items are zero because linear memory starts zeroed, and runtime globals have constant initializers (§10.1.3).
3. No SAI code runs at instantiation. SAI has no static initializers: constants are evaluated by the compiler ([10](../spec/10-errors-and-panics.md) §10.4.6), and there is no mutable static state (D-CAP-02).

A host can therefore instantiate a module, or a component built from it, in any order with respect to other instances, and instantiation never traps for reasons of SAI.

### 10.7.2 Entry points

Execution starts only at an entry point:

| Artifact | Entry points | Defined in |
|---|---|---|
| core module | the wrappers of the root's `export fn` items | [06](06-wasm-lowering-and-layout.md) §6.6.7 |
| command component | the export glue of `wasi:cli/run`, which calls `main` with the `process.Env` value | [07](07-component-lowering.md) §7.8.4, [targets-and-profiles.md](../targets/targets-and-profiles.md) §4 |
| reactor component | the export glue of each exported interface function | [07](07-component-lowering.md) §7.8 |
| any component | `cabi_realloc` and the post-return functions, called by the host | §10.3, [07](07-component-lowering.md) §7.5.5 |
| test harness | the exports `test.<k>`, `case.<k>` and `counterexample.<k>`, called by the runner | [impl/13](13-cli.md) §13.6.6.2, [04](04-mir.md) §4.13.10 |

The runtime adds no entry point and no wrapper around entry points. The first allocation of an instance initializes nothing lazily: the allocator's state is valid at instantiation (all heads 0, `heap_top = __heap_base`).

### 10.7.3 Successive calls

An instance keeps its state between calls into it: allocator state, live heap blocks owned by the host through handles or exported resources, and the memory size. The runtime state is valid at every point between calls, because every runtime function restores its invariants before returning. No runtime function keeps state across calls other than the items of §10.1.3.

### 10.7.4 After a trap

A trap ends the instance ([10](../spec/10-errors-and-panics.md) §10.7.2). The runtime restores nothing: `__sp` keeps its value, an allocator call that was interrupted by an out-of-memory panic leaves its state as it was before the call (§10.2.6), and `__sai_panicking` stays 1 if the trap came from a panic. A host MUST NOT call into the instance again, including `cabi_realloc` (N-C-83). If it does, the runtime gives the call no meaning. The guard of §10.5.8 ensures only that such a call cannot report a second panic.

---

## 10.8 Async (Stage 5, provisional)

### 10.8.1 No runtime support in v0.1

Async code needs no runtime component in v0.1 (D-ASYNC-01):

- Futures are state machines produced by impl/04 ([11](../spec/11-async-and-concurrency.md) §11.10.5). They are ordinary values in frames or in heap blocks, allocated through the ordinary allocator.
- Executors, wakers, `scope`, `join`, `select` and the reactor are library code of `sai:async` ([11](../spec/11-async-and-concurrency.md) §11.9.3). They are linked only when reachable, like any library code. The runtime has no scheduler, no task queue and no runtime state for async code.
- On WASI targets the reactor waits through `wasi:io/poll`, an ordinary WASI import that the capability of every WASI effect implies ([11](../spec/11-async-and-concurrency.md) §11.10.2, [targets-and-profiles.md](../targets/targets-and-profiles.md) §3). On core targets there are no pollables, and `block_on` reports every wait as a deadlock ([11](../spec/11-async-and-concurrency.md) §11.9.2).

### 10.8.2 The Component Model async ABI

No v0.1 target provides the Component Model async ABI ([targets-and-profiles.md](../targets/targets-and-profiles.md) §3). An exported interface with an `async fn` is therefore `ASYNC_EXPORT_UNSUPPORTED` on every target ([11](../spec/11-async-and-concurrency.md) §11.10.4), and v0.1 has no runtime code for that ABI. When a target adds the ABI, the provisional mapping of [11](../spec/11-async-and-concurrency.md) §11.10.3 is implemented as follows:

1. The callback entry and `task.return` are export glue of impl/07, not runtime functions.
2. The waitable set of an export task is created by `sai:async` code when the task first registers a waitable. Its handle is stored in the task's future, not in runtime state, so concurrent export tasks of one instance need no global table.
3. A WASI 0.2 pollable is waited on through the host's adapter between the two models, which `sai:async` reaches as ordinary imports. The runtime adds nothing.

The runtime therefore stays free of async state on every target. That is the intent of D-ASYNC-01: "the scheduler is a library, linked only when used".

---

## 10.9 Threads

Threads are not implemented in v0.1 (D-THREAD-01). The memory is never shared ([06](06-wasm-lowering-and-layout.md) §6.3.7), and the runtime uses no atomic instruction. The allocators and the panic reporter assume that exactly one call into the instance is running, with reentrant calls only through imports, which nest. A future target with threads needs a thread-safe allocator, a per-thread shadow stack and a per-thread `__sai_panicking`, and none of the algorithms of this chapter can be reused unchanged.

---

## 10.10 Runtime size (informative)

The runtime is small because each component is linked only when used (§10.1.4) and because the algorithms avoid generality that most programs do not need. The following upper bounds for the code section contribution on wasm32 are checked by the test suite (§10.11, N-F-38):

| Component | Profile of measurement | Code bytes at most |
|---|---|---|
| default allocator, with `__sai_oom` in `TrapOnly` mode | `release` | 1536 |
| tiny allocator, with `__sai_oom` | `tiny` | 640 |
| allocator package glue (§10.2.7), without the package's own code | `release` | 384 |
| `cabi_realloc` on top of an allocator | `release` | 96 |
| panic reporter with the hook channel, without panic sites | `release` | 1024 |
| panic reporter with the stderr channel, without panic sites | `release` | 1152 |

Static data adds 45 bytes of fragments for the reporter, 28 bytes per panic site, and the file strings and fixed texts. A `TrapOnly` build adds none of these. `sai size` attributes every runtime function to the group `Runtime features`, under `allocator` or `panic`, except the functions of the in-module handle table, which belong to `SAI code` like the other boundary code of the build ([09](09-optimization-and-size.md) §9.10, N-F-96).

The draft's example `export fn add(a: I32, b: I32) -> I32 { a + b }` on `wasm32-core` contains no runtime at all, in every profile: it reaches no allocation, and without a `@panic_hook` its overflow check is `unreachable` (draft §2.3, [06](06-wasm-lowering-and-layout.md) §6.3.4).

---

## 10.11 Testing

The reference implementation's test suite MUST contain the following tests of the runtime. They run in the embedded runtime of `sai test` ([13](13-cli.md) §13.6.6), on the `wasm32-core` and `wasm64-core` targets and on `wasm32-wasi`.

1. **Allocator model tests.** For each built-in allocator, seeded random sequences of `__sai_alloc`, `__sai_realloc` and `__sai_free` calls, with sizes drawn from 0 to 70,000 bytes and alignments from 1 to 64, are run in Wasm and in a host-side model that executes the pseudocode of §10.2 literally. Every returned address MUST equal the model's (§10.1.5), and after every call a checker reading linear memory MUST confirm the invariants of §10.2.4 or §10.2.5 and the preservation of block contents.
2. **Out of memory.** With the engine's memory limit set to a small number of pages, an allocation loop MUST end with the report `panic: out of memory` in `Report` mode and with a trap and no output in `TrapOnly` mode. Requests of `ISIZE_MAX` bytes and of `ISIZE_MAX - 8` bytes with alignment 16 MUST take the same path.
3. **Allocator packages.** The package of §10.2.8 MUST build and run the allocator model sequences correctly (with addresses compared only for alignment and disjointness). One test package per error condition of §10.2.7 MUST produce `ALLOCATOR_INTERFACE_MISMATCH` or `ALLOCATOR_REENTRANCY` at the expected spans.
4. **`cabi_realloc`.** Each row of the table of §10.3.1, and alignments 0, 3 and 16, MUST give the specified result or trap.
5. **Arenas.** The chunk sequence of §10.4.4 MUST appear in a trace of allocator calls, and an arena that never allocates MUST make no allocator call.
6. **Panic reports.** For every kind of [10](../spec/10-errors-and-panics.md) §10.4.4 and both channels, the delivered bytes MUST equal a golden file, byte for byte, in `dev` and `release`. Messages of 1023, 1024, 1025, 70,000 and 300,000 bytes MUST be delivered untruncated. On the stderr channel, a report of 10,000 bytes MUST arrive as three writes, and a host whose stream returns an error on the first write MUST observe exactly one write and then a trap.
7. **Stack overflow.** Unbounded recursion of a function with a frame MUST deliver `panic: stack overflow` in `Report` mode.
8. **Reentrancy.** A hook that calls an export which panics MUST observe exactly one call of the hook, and the instance MUST trap.
9. **Absence.** The core module of `export fn add` MUST contain no `__sai_` function, no runtime state and no runtime global, in every profile. The core module of every `TrapOnly` build MUST contain no panic reporter: no `__sai_panic`, `__sai_stack_overflow`, `__sai_panic_buf`, `__sai_panic_ret` or `__sai_panicking`, and no panic site record. A program that allocates nothing MUST contain no allocator, in every profile.
10. **Size bounds.** The bounds of §10.10 MUST hold for the reference programs of the size test suite ([09](09-optimization-and-size.md) §9.11).
11. **Handle table.** Seeded random sequences of `__sai_handle_new`, `__sai_handle_rep`, `__sai_handle_lend`, `__sai_handle_unlend`, `__sai_handle_move` and `__sai_handle_drop` calls with two type indices are run in Wasm and in a host-side model of the Component Model's resource table. Every returned handle and rep MUST equal the model's, and every call for which the model traps MUST trap: index 0, an index never created, a freed index, the other type index, and a move or drop while lent. The sequence new, new, drop 1, new MUST return the handles 1, 2 and 1. A program whose build provides no resource MUST contain no `__sai_handle_` function and no `__sai_handles` item (§10.3.3).

---

## 10.12 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-F-23 | Runtime functions are Mono MIR bodies built by the compiler, not SAI source. They have no shadow-stack frame, call no program code other than an allocator package and the reporting channel, contain no `Assert`, and do not recurse except through `__sai_panic` (§10.1.2). | They need runtime globals, `memory.grow` and linker symbols, which SAI cannot express (D-CAP-02). Frame-less bodies work after a stack overflow, and P8 is easier to meet with one pipeline. |
| N-F-24 | Runtime state is the zero-initialized items `__sai_alloc_heads`, `__sai_tiny_head`, `__sai_custom_state`, `__sai_panic_buf` and `__sai_panic_ret`, placed after static data in this order. The runtime globals are `__sai_heap_top` and `__sai_panicking`. Each exists only when its component is linked (§10.1.3). | Zero-initialized memory costs no module bytes and needs no initialization code (N-F-37). A fixed order makes addresses reproducible (P8). |
| N-F-25 | Every allocator returns `align` for size 0, never returns 0, never returns after a failure, calls no host function, traps on an alignment that is not a power of two, and is a deterministic function of the call sequence (§10.2.2, §10.2.3). | One contract for all allocators keeps the choice of allocator from changing meaning (C-03, P8). The trap turns a *Safety* violation into a defined failure at a cost of a few instructions. |
| N-F-26 | The allocator is the root's `allocator` package in every profile if declared, otherwise the tiny allocator in `tiny` and the default allocator in `dev` and `release` (§10.2.1). | The explicit declaration is the program's choice (P1). Profile defaults follow D-TGT-02. |
| N-F-27 | The default allocator has eight size classes from 16 to 2048 bytes, served from free lists of never-split blocks, and one address-sorted first-fit list of large blocks that is split from the end and coalesced. Alignments of 32 or more are over-allocated (§10.2.3, §10.2.4). | Small, predictable code with bounded work for small blocks (P3, P4). Coalescing only large blocks bounds fragmentation where it costs memory. |
| N-F-28 | The tiny allocator is a bump allocator with one LIFO free list that is split from the end, and it returns a block at the top of the heap to the bump pointer (§10.2.5). | The smallest correct allocator that reuses memory, for the `tiny` profile (D-WASM-05). |
| N-F-29 | `carve` is the allocators' only source of fresh memory. It grows memory by exactly the pages needed, never shrinks it, keeps the heap below `ADDR_LIMIT`, and calls `__sai_oom`, which panics with `OutOfMemory` in `Report` mode and traps in `TrapOnly` mode (§10.2.6). | One growth point makes the overflow argument local. Exact growth keeps memory use minimal and deterministic (P8). |
| N-F-30 | An allocator package declares `STATE_SIZE` and the four `unsafe fn`s `add_region`, `allocate`, `deallocate` and `reallocate` with fixed signatures. They have no effects and cannot reach an allocation primitive. The package is used in source form. The runtime retries once after granting a region from `carve` (§10.2.7). | SAI code cannot execute `memory.grow` (no intrinsic exists), so the runtime grants regions. Excluding reentrancy statically avoids a run-time guard (P3). |
| N-F-31 | `cabi_realloc` traps on an alignment outside `{1, 2, 4, 8}` and otherwise is `__sai_realloc`. A zero size returns `align` (§10.3). | The Canonical ABI uses only those alignments. Reusing `__sai_realloc` adds only the alignment check (P3). |
| N-F-32 | An arena is library code of `sai:alloc`. Its value is `{chunk, cursor, limit}` and its chunks have a 16-byte header. The first chunk is 4096 bytes and chunk sizes double up to 65536. A request larger than the next chunk gets a dedicated chunk of `align_up(16 + size, 16)` bytes. `region_new` allocates nothing, and `drop` frees all chunks (§10.4). | Arenas need no runtime support (P3). Doubling bounds the number of allocator calls for large regions, and lazy allocation keeps unused regions free. |
| N-F-33 | A panic site is a static record of 7 fields (28 bytes on wasm32, 48 on wasm64), with forms `Text`, `Bounds` and `Dynamic`, and a file string `<ns>:<name>@<version>/<path>` (§10.5.2). | Sites make the panic sequence one call with three arguments (impl/06 §6.8.12) and keep messages as static data. The version disambiguates coexisting lines (D-PKG-02). |
| N-F-34 | A report is `panic at <file>:<line>:<column>: <message>` or `panic: <message>`, followed by a line feed, formatted by the runtime without allocation (§10.5.5). | One fixed line is easy for hosts and tools to parse, and is identical in `dev` and `release` ([10](../spec/10-errors-and-panics.md) §10.5.2). |
| N-F-35 | A report of up to 1024 bytes uses `__sai_panic_buf`. A longer one is written above `heap_top`, growing memory if needed, and a failure to grow traps without a report (§10.5.5). | Messages are never truncated and the reporter never allocates ([10](../spec/10-errors-and-panics.md) §10.4.4, §10.5.2). The common case costs no growth. |
| N-F-36 | The stderr channel calls `get-stderr` once and `blocking-write-and-flush` in pieces of at most 4096 bytes, stops at the first error, and drops no handle (§10.5.4). | WASI 0.2 limits one write to 4096 bytes. The instance traps right afterwards, so cleanup would be dead code ([10](../spec/10-errors-and-panics.md) §10.5.3). |
| N-F-37 | A module has no start function and no initialization export, and instantiation runs no code: all runtime state is valid when zero, and runtime globals have constant initializers (§10.7.1). | SAI has no static initializers (D-CAP-02). No start code means smaller modules and instantiation that cannot trap (P3, P4). |
| N-F-38 | The test suite checks the code-size bounds of §10.10 and the tests of §10.11 (§10.10, §10.11). | Size is a requirement of the `tiny` profile (D-TGT-02), and a bound that is not tested regresses (P8). |
| N-F-82 | A test harness reports a panic through the harness channel, the core import `sai-harness` `panic` with the address and the length of the report, called once per instance. Its panic mode is `Report` under `dev` and `release` and `TrapOnly` under `tiny`, whatever the target (§10.5.3, §10.5.4). | [spec/14](../spec/14-testing.md) §14.4.2 requires a channel in every test build that is neither the program's hook nor standard error. One private import with the format of N-F-34 lets the runner parse every report the same way, and it adds no capability (P5, P8). |
| N-F-95 | The handles of resources provided within the build live in the in-module handle table: the runtime-state item `__sai_handles` (item 6, after the items of N-F-24) with the fields `base`, `cap`, `len` and `free`, and entries of `PS + 8` bytes holding `rep`, `kind` and `lends`. Index 0 is reserved, freed indices are reused last freed first, the array doubles from 4 entries up to `2^28 - 1`, and the six operations trap exactly where the Component Model's resource table traps (§10.3.3). | Core targets have no resource table, and the same table on every target keeps one implementation of provision ([07](07-component-lowering.md) N-D-108, P4). Mirroring the Component Model's table and checks makes handle values and traps independent of the form of the hybrid package (P8). |
| N-F-96 | The handle table is a runtime component that is linked only when provision glue calls it, and `sai size` attributes its functions to `SAI code` (§10.1.1, §10.1.4, §10.10). | A program without provided resources pays nothing (D-WASM-05). The table serves the boundary of the build as a whole, not one provider, so it belongs with the other boundary code (P8). |

---

## 10.13 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `ALLOCATOR_INTERFACE_MISMATCH` | error | the package named by `allocator` does not declare the items of §10.2.7 with the required names, kinds and signatures, a function has effects, is generic, `async` or has a contract, `STATE_SIZE` is outside `0..=4096`, or the package is not used in source form (§10.2.7) |
| `ALLOCATOR_NOT_REQUIRED` | error | defined in [01-package-contract.md](../package/01-package-contract.md) §1.15; the `allocator` package is not a requirement of the root (§10.2.1) |
| `ALLOCATOR_REENTRANCY` | error | a function of the allocator package can reach an allocation primitive of `alloc.heap`, including through drop glue (§10.2.7 item 2) |
| `ASYNC_EXPORT_UNSUPPORTED` | error | defined in [11](../spec/11-async-and-concurrency.md) §11.10.4; an exported interface with an `async fn` on a target without the Component Model async ABI, which is every v0.1 target (§10.8.2) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [01-introduction.md](../spec/01-introduction.md) §1.10 and reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6; a runtime body with a shadow-stack frame (§10.1.2 rule 1) |
| `MISSING_SAFETY_DOC` | warning (lint) | defined in [12](../spec/12-unsafe-and-ffi.md) §12.2.2; a function of an allocator package without a `# Safety` section (§10.2.7) |
