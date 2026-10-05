# 07. Ownership and Borrowing

**Status:** Normative
**Stage:** 2 (resources: Stage 4; borrows held across `.await`: Stage 5; raw pointers: see 12)
**Related:** [decisions.md](../design/decisions.md) D-OWN-01..07, D-TYPE-02, D-TYPE-04, D-TYPE-08, D-TYPE-09, D-EXPR-02, D-EXPR-04, D-ASYNC-01; [05-types.md](05-types.md), [06-expressions-and-statements.md](06-expressions-and-statements.md), [11-async-and-concurrency.md](11-async-and-concurrency.md), [12-unsafe-and-ffi.md](12-unsafe-and-ffi.md), [13-components-and-wit.md](13-components-and-wit.md), [16-core-library.md](16-core-library.md), [../impl/05-borrow-checker.md](../impl/05-borrow-checker.md)

This chapter defines the ownership discipline of SAI. It covers places, moves and copies, initialization state, explicit borrows, reborrowing, regions and their elision in signatures, the `borrows { .. }` clause, view types and region-carrying records, the declarative borrow rules on a control-flow model (non-lexical, liveness-based), two-phase receiver borrows, closures and captures, drop and drop order, resources, `region` arenas, the ownership side of `.await`, and the boundary with unsafe code. The rules here are declarative: they say which programs are accepted. The reference algorithm that decides them on MIR is in [impl/05](../impl/05-borrow-checker.md). That algorithm MUST accept exactly the programs this chapter accepts and MUST report the diagnostics this chapter names.

Types, traits, which types are Copy, method resolution, and coercion sites belong to [05](05-types.md). Expression evaluation order, patterns, and temporary scopes belong to [06](06-expressions-and-statements.md). This chapter restates only what it needs from them and links to the owner.

---

## 7.1 Conventions

### 7.1.1 Terminology

- A **borrow region** (or just **region**) is a set of program points during which a borrow MUST stay valid. Regions are written `'a`, `'b`, `'1`, ... in this specification and in diagnostics. They have **no source syntax** (D-OWN-03). A `'name` token in source is always a loop label (02 §2.6.3).
- An **allocation region** is the arena introduced by a `region r { .. }` block (§7.15). The two terms are never used interchangeably (D-OWN-07). When this chapter says "region" without qualification, it means a borrow region.
- A **view type** is a type that contains a borrow: `Str`, `Slice<T>`, `SliceMut<T>`, `borrow T`, and `borrow mut T` (D-TYPE-02). Every view type carries exactly one region of its own.
- A type **carries a region** if a region occurs in it after type arguments are substituted (§7.6.2).
- A **loan** is the record of one borrow: the borrowed place, the kind (shared or mutable), its region, and the point that created it (§7.9.3).
- **Drop** is the destruction of an owned value: running its `Drop` impl, if any, and then dropping its components (§7.13).

### 7.1.2 Judgment forms

This chapter uses these judgment forms, in addition to the typing judgment `Γ ⊢ e : T ! E` of 05:

| Form | Meaning |
|---|---|
| `Γ ⊢ p place : T` | `p` is a place expression of type `T` (§7.2) |
| `Γ ⊢ p mutable` | the place `p` may be written and borrowed mutably (§7.2.4) |
| `'a : 'b` | region `'a` outlives `'b`: as point sets, `'a ⊇ 'b` (§7.6.1) |
| `regions(T)` | the set of regions that occur in `T` (§7.6.2) |
| `init(P, p)` | the place `p` is definitely initialized at point `P` (§7.4) |
| `live(P, x)` | the local `x` is live at point `P` (§7.9.4) |
| `inscope(P, L)` | the loan `L` is in scope at point `P` (§7.9.5) |

### 7.1.3 Relationship to the reference algorithm

The rules in §7.4 and §7.9 are stated on an abstract control-flow graph (CFG) of program points (§7.9.1). The CFG is the one produced by the MIR construction of [impl/04](../impl/04-mir.md). Its shape follows from the evaluation order of 06 (D-EXPR-02) and the temporary scopes of 06. Acceptance MUST depend only on the procedures defined here. It MUST NOT depend on optimizer strength or on heuristics (the determinism principle, decisions §0.1). In particular, the checker does not reason about the values of integers, so `v[0]` and `v[1]` are treated as overlapping places (§7.2.3).

### 7.1.4 Example conventions

To keep them short, the examples assume these helper functions. Library calls such as `String.new()`, `"a".to_string()`, `v.push(x)`, `v.len()`, and `core.mem.take` are the ones of [16](16-core-library.md).

```sai
fn consume(s: String) {}
fn print_len(s: borrow String) {}
fn touch(v: borrow mut Vec<I32>) {}

fn make_string() -> String {
    String.new()
}

// The result borrows from `s` (§7.7.2 rule O3).
fn first_word(s: borrow String) -> Str {
    s.as_str()
}

fn use_str(s: Str) {}
```

`User` and `Image` stand for record types, `File` for the resource declared in §7.14, and `Source` for a type with an `async fn read(self: borrow Self) -> USize` method, each with the fields the example uses.

A line marked `// rejected: CODE` MUST be rejected with that diagnostic code, and the line contains the diagnostic's primary span (§7.18.1). No other line of the example produces an error. An example without a `rejected` line MUST be accepted.

---

## 7.2 Places

### 7.2.1 Place expressions

A **place expression** denotes a memory location. These are the place expressions:

1. A local variable, a function parameter (including `self`), or a pattern binding.
2. `p.f`, a field of a place `p` whose type is a record type (D-SYN-01), a record variant reached through a pattern, or a closure environment (§7.11).
3. `p.0`, `p.1`, ..., a tuple element of a place `p`.
4. `*e`, where `e` has type `borrow T`, `borrow mut T`, or `Box<T>`. For `Ptr<T>` and `PtrMut<T>` it is allowed only in unsafe code (12, §7.17).
5. `p[i]`, where `p` is a place of type `Array<T, N>`, `Slice<T>`, or `SliceMut<T>`. This is a built-in place (D-EXPR-10). `i` is any expression of type `USize`, or a range for subslicing (06).
6. `e[i]` on a type that implements `Index` / `IndexMut` (for example `Vec`, `String`, `Str`). This is the place `*Index.index(borrow e, i)` in a read context, and the place `*IndexMut.index_mut(borrow mut e, i)` in a write or `borrow mut` context (16). Such a place is always **behind a borrow** (§7.3.5).
7. Auto-deref: `e.f`, where `e` has type `borrow T`, `borrow mut T`, or `Box<T>`, is the place `(*e).f`. Auto-deref repeats until a type with field `f` is reached (D-OWN-02). There is no user-defined `Deref`.

Every other expression is a **value expression**. When a value expression appears where a place is required (the operand of `borrow`, the base of a field access, or the receiver of a method), it is evaluated into a fresh **temporary**, and the temporary is the place. Temporary scopes are defined in 06. The one lifetime extension is in §7.5.6.

A constant item (D-CAP-02) is a value expression, not a place. Each use of a constant produces a fresh value, which is never a move. `borrow C` for a constant `C` borrows a temporary. There is no static promotion (N-B-1).

### 7.2.2 Canonical form

Every place has the canonical form `x.π`. Here `x` is a local (its **root**) and `π` is a possibly empty sequence of **projections**:

| Projection | Written | Introduced by |
|---|---|---|
| `Field(f)` | `.f`, `.0` | field and tuple-element access, record patterns |
| `Deref` | `*` | explicit `*`, auto-deref, the desugaring of `Index` |
| `Index(i)` | `[i]` | indexing an Array, Slice, or SliceMut with an index expression (constant or not) |
| `ConstIndex(k, from_end)` | — | slice patterns (`[a, b, ..]`, `[.., z]`) |
| `Subslice(from, to)` | — | the `..rest` element of a slice pattern |
| `Downcast(V)` | — | matching the enum variant `V` in a pattern |

These projections correspond one-to-one to the MIR projections of D-MIR-01.

An element or subslice of a `Slice<T>` or `SliceMut<T>` value `s` is canonicalized as `(*s)[i]`: a `Deref` of the view followed by the element projection. The elements live in the viewed data, not in `s`. An element of an `Array` local `a` is `a[i]`, with no `Deref`.

A place is **owned along its path** if no projection in `π` is a `Deref` of a view type or of a raw pointer. A `Deref` of `Box<T>` keeps a place owned, because a `Box` owns its contents. A place that has a `Deref` of `borrow T`, `borrow mut T`, `Ptr<T>`, or `PtrMut<T>` in its path is **behind a borrow** (or behind a raw pointer).

### 7.2.3 Prefixes, overlap, and disjointness

- `q` is a **prefix** of `p` if they have the same root and the projections of `q` are an initial segment of the projections of `p`. Every place is a prefix of itself.
- Two places are **disjoint** if they have different roots, or if they share a longest common prefix `c` and their next projections after `c` are one of these pairs:
  - two different fields of the same record, tuple, or enum variant;
  - two `ConstIndex` projections with different resolved positions in the same Array (for a Slice or SliceMut, two `ConstIndex` projections are disjoint only if both count from the same end);
  - two `Downcast` projections to different variants.
- Two places **overlap** if they are not disjoint. In particular, `Index(i)` overlaps every other `Index`, `ConstIndex`, or `Subslice` of the same base, whatever the indices are (§7.1.3). A `Subslice` overlaps every element projection of the same base.

Overlap is computed on canonical places after auto-deref. Two different roots never overlap, even if both are borrows of the same data. That case is covered by the loans the borrows hold (§7.9).

### 7.2.4 Mutability of places

`Γ ⊢ p mutable` holds by these rules:

1. A local `x` is mutable if it was declared with `var`: a `var` statement, a `var` pattern binding, or a `var` parameter (D-SYN-10, D-SYN-06).
2. `p.f`, `p.0`, a `ConstIndex`, a `Subslice`, and a `Downcast` of `p` are mutable if `p` is mutable. So is `p[i]` when `p` is an `Array`. An element of a `SliceMut<T>` is `(*s)[i]` (§7.2.2), so it is mutable when `*s` is (rule 4, with `SliceMut<T>` treated like `borrow mut`). An element of a `Slice<T>` is not mutable (rule 5).
3. `*e`, where `e : Box<T>` is a place, is mutable if `e` is mutable.
4. `*e`, where `e : borrow mut T`, is mutable if the place `e` is **unique**. A place is unique if its path has no `Deref` of a shared view (`borrow T`, `Str`, `Slice<T>`) or of a raw pointer. The mutability of the binding that holds `e` does not matter. A `let r = borrow mut x` lets `r` write `*r`. It does not let `r` be reassigned. With `rr : borrow (borrow mut T)`, the place `**rr` is not mutable, because `*rr` is behind a shared borrow.
5. `*e`, where `e : borrow T`, `Str`, or `Slice<T>`, is not mutable.
6. `*e`, where `e : PtrMut<T>`, is mutable in unsafe code. `*e`, where `e : Ptr<T>`, is not mutable (12).
7. An `IndexMut` place `e[i]` (§7.2.1 rule 6) is mutable if the place `e` can be borrowed mutably. That is, `borrow mut e` must be valid, which (by these rules) requires `e` to be mutable, or `e` to auto-deref through a `borrow mut`.
8. A temporary is mutable.

Writing to a place that is not mutable is `ASSIGN_TO_IMMUTABLE`. Borrowing it mutably is `BORROW_MUT_OF_IMMUTABLE` (§7.5.1). Both are reported with a structured fix (§7.18):

- When the root is a `let` local and the path has no `Deref` of a shared view: "change `let` to `var`".
- When the place is behind a `borrow T` that comes from a parameter: "change the parameter type to `borrow mut T`".
- Otherwise there is no machine-applicable fix.

---

## 7.3 Ownership, moves, and copies

### 7.3.1 Owners

Every value has exactly one owner at any time. The owner is one of these:

- a local (including a parameter or a pattern binding)
- a field or element of an owned value
- the heap contents of a `Box`, `Vec`, or `String` (owned through that container)
- a temporary
- an arena (§7.15)

When the owner stops being initialized (because it was moved from, or went out of scope), ownership is transferred or the value is dropped (§7.13).

### 7.3.2 Uses: copy or move

A place expression `p` of type `T` that appears in a **value context** is a **use** of `p`:

- If `T` is Copy (05; D-TYPE-08), the use is a **copy**: a read of `p` that leaves `p` initialized.
- Otherwise the use is a **move**: a move access of `p` (§7.9.2). After the move, `p` is uninitialized (§7.4).

These are the value contexts:

- an argument of a call
- the receiver of a method whose `self` type is `Self` (§7.5.3)
- the initializer of a `let` or `var` statement whose pattern binds the whole value by value
- the right-hand side of an assignment
- the operand of `return` or `break`, and the tail expression of a block
- a field initializer in record construction, and an element of a tuple or array literal
- the operand of `.await` (Stage 5)
- an operand of an operator whose desugaring takes it by value (06, 16)
- a by-move capture (§7.11)

These are **place contexts**, which neither copy nor move:

- the operand of `borrow` / `borrow mut`
- the left-hand side of an assignment
- the base of a field access or index
- the receiver of a method whose `self` type is `borrow Self` or `borrow mut Self`
- the scrutinee of `match`, `if let`, and `while let`
- the initializer of `let` when the pattern does not bind the whole value by value

For a pattern matched against a place `p` (in `let`, `match`, or `if let`), each part of `p` is treated according to the pattern (06 owns the pattern semantics; D-EXPR-04):

- A by-value binding (`x` or `var x`) of a part of non-Copy type moves that part. A by-value binding of a part of Copy type copies it.
- A `borrow x` binding creates a shared loan of that part. A `borrow mut x` binding creates a mutable loan, and requires the part to be mutable (§7.2.4).
- `_`, `..`, literal patterns, and constructor tests only read the discriminant or the compared value. They move nothing.

So `let _ = x` neither moves nor drops `x`. `let (a, _) = pair` moves only `pair.0`. When the scrutinee is a value expression, it is a temporary, and the parts that are not bound are dropped with that temporary (06, §7.13.3).

### 7.3.3 Copy types

Which types are Copy is defined by 05. In summary (D-TYPE-02, D-TYPE-08):

- Scalars are Copy.
- Shared views (`Str`, `Slice<T>`, `borrow T`) are Copy.
- Exclusive views (`SliceMut<T>`, `borrow mut T`) are not Copy.
- Tuples and Arrays are Copy when all their elements are.
- A nominal type is Copy only if Copy is derived explicitly.
- Resources are never Copy (§7.14).

A Copy type never has drop glue. A type that implements `Drop` cannot be Copy (`COPY_AND_DROP`, §7.13.6).

Copying a value of a view type duplicates the borrow, not the data it points to. The copy carries the same region (§7.8).

### 7.3.4 `owned T`

`owned T` is a synonym of `T` in every type position (D-OWN-01; 03 §3.15). It is documentation only. The type checker treats `owned T` and `T` as the same type, and no diagnostic depends on whether `owned` was written. `owned borrow T` is the type `borrow T`.

```sai
fn keep(file: owned File) -> File { // `owned File` and `File` are the same type
    file
}

fn close(file: owned File) {} // takes ownership; `file` is dropped at the end
```

### 7.3.5 What can be moved

The following table says whether a non-Copy place `p` can be moved, judged by the last projection that matters:

| Place being moved | Allowed? | Diagnostic and fix |
|---|---|---|
| a whole local `x` | yes | — |
| a field path `x.f.g` that is owned along its path (§7.2.2), where no type along the path implements `Drop` | yes (a **partial move**) | — |
| a field of a value whose type implements `Drop` | no | `MOVE_OUT_OF_DROP_TYPE`; fix: `core.mem.replace` / `core.mem.take`, or `.clone()` |
| any place behind a borrow (`*r`, `r.f` with `r : borrow T` or `borrow mut T`, an `Index`/`IndexMut` place) | no | `MOVE_OUT_OF_BORROW`; fix: `.clone()`, `core.mem.replace(borrow mut p, v)`, `core.mem.take(borrow mut p)`, or bind by `borrow` in a pattern (D-OWN-01, D-EXPR-04) |
| `*b` or a field of `*b`, where `b : Box<T>` | no (N-B-2) | `MOVE_OUT_OF_BOX`; fix: `Box.into_inner(b)` |
| `a[i]` on an owned `Array` (built-in index) | no (N-B-4) | `MOVE_OUT_OF_INDEX`; fix: `core.mem.replace`, or a slice pattern |
| a `ConstIndex` element, or a `Subslice` rest, of an owned `Array` local, bound by a slice pattern | yes (a partial move, N-B-28) | — |
| a place behind a raw pointer | no | `MOVE_OUT_OF_BORROW`; fix: `p.read()` (`Ptr.read`, 16 §16.12.2) in unsafe code (12) |
| a by-reference capture inside a closure body | no; see §7.11.2, which changes the capture to by-move instead | — |

"A type along the path" means the type of each strict prefix of `p` that is projected through a `Field`, `ConstIndex`, or `Downcast`. Moving `x.a.b` requires that neither the type of `x` nor the type of `x.a` implements `Drop`.

The rules for `Box` (N-B-2) and indexing (N-B-4) are decisions of this chapter:

- **`Box` (N-B-2):** a `Box` is an ordinary library type that implements `Drop`, so moving out of its contents follows the same rule as for any other `Drop` type. Its contents can be read through `*b` when they are Copy, borrowed through `borrow *b` / `borrow mut *b`, and moved out only with `Box.into_inner(b)`.
- **Indexing (N-B-4):** the checker does not track individual dynamic indices (§7.1.3), so moving out of `a[i]` would leave a hole that it cannot follow.

```sai
type Pair {
    left: String
    right: String
}

fn split(p: Pair) -> String {
    let l = p.left          // partial move of p.left
    consume(p.right)        // partial move of p.right
    l                       // accepted: p is fully moved, so nothing of p is dropped
}

fn first(v: borrow Vec<String>) -> String {
    (*v)[0]                 // rejected: MOVE_OUT_OF_BORROW (fix: `(*v)[0].clone()`)
}

fn take_name(user: borrow mut User) -> String {
    core.mem.take(borrow mut user.name)    // accepted: leaves String.default() behind
}
```

### 7.3.6 Assignment, reinitialization, and the old value

An assignment `p = e` is evaluated as follows (D-EXPR-02):

1. `e` is evaluated first, then the place `p`.
2. `p` MUST be a place expression (otherwise `INVALID_ASSIGN_TARGET`, 03 §3.16) and MUST be mutable (§7.2.4), otherwise `ASSIGN_TO_IMMUTABLE`. A `let` local is never assignable, not even after it has been moved out of.
3. Every strict prefix of `p` that is owned along its path MUST be definitely initialized (§7.4). Assigning to a field of a value that has been moved out as a whole, or whose enclosing field has been moved out, is `PARTIAL_ASSIGN_TO_MOVED` (N-B-3). The state of each prefix is its own state as a move path (§7.4.2): after a move of `x.g` alone, `x` is still initialized, so `x.f = v` is accepted. Assigning to exactly the moved-out path reinitializes it. If `p` goes through a `Deref` or an `Index`, the place before that projection is used (it is read to find the target), so it must satisfy §7.4.4 item 1.
4. If `p` is maybe-initialized, its old value is dropped at this point (with a drop flag if the state is only known at run time, §7.13.4). If `p` is definitely uninitialized, nothing is dropped.
5. The new value is written to `p`. `p` and all its sub-paths become initialized.

A compound assignment `p op= e` is `{ let t = e; p = p op t }` with the subexpressions of `p` evaluated once (D-EXPR-02). It is a read of `p` followed by a write of `p`.

```sai
fn reinit() {
    var s = "a".to_string()
    consume(s)
    s = "b".to_string()          // accepted: reinitializes s; there is no old value to drop
    consume(s)
}

fn bad_reinit(p: Pair) {
    var q = p
    consume(q)
    q.left = "x".to_string()     // rejected: PARTIAL_ASSIGN_TO_MOVED (q is not initialized)
}
```

---

## 7.4 Initialization state

### 7.4.1 Move paths

A **move path** is a place that is owned along its path (§7.2.2) and has no `Index` projection. Each local has a tree of move paths: the root, its fields, and so on, down to the places that are actually moved, assigned, or borrowed. The checker tracks initialization per move path. A `ConstIndex` and a `Downcast` each form a move path. A `Subslice` of an owned `Array` (the `..rest` binding of a slice pattern, which moves an `Array<U, N-k>`, 06 §6.21.3) stands for the `ConstIndex` paths of the positions it covers (N-B-28). Places behind a borrow are not move paths: they are never moved out of (§7.3.5) and never uninitialized.

### 7.4.2 Events

On every CFG point, a move path `m` is affected by these events:

- **Initialized by:** a write of `m` or of a prefix of `m` (assignment, `let` / `var`, a pattern binding, the call-return destination, or parameter entry).
- **Uninitialized by:** a move of `m` or of a prefix of `m`, the storage end of its root (`StorageDead`), or the drop of `m` or of a prefix of `m`.

A move of `x.f` uninitializes `x.f` and every path below it. It does not uninitialize `x` or its other fields. `x` is then **partially moved**.

### 7.4.3 Definite initialization

`init(P, m)` holds if, on every CFG path from the function entry to `P`, the last event affecting `m` is an initialization. Every move path is uninitialized at function entry except the parameters, which are initialized.

`m` is **maybe-initialized** at `P` if, on some path, the last event affecting `m` is an initialization. It is **maybe-uninitialized** at `P` if, on some path, there is no such event, or the last one is an uninitialization.

### 7.4.4 Requirements on uses

1. A copy, move, or borrow of a place `p` requires `init(P, m)` for:
   - the longest move path `m` that is a prefix of `p`, and
   - every move path below `p`.

   If `p` is behind a borrow, the requirement applies to the borrow that is dereferenced, which is itself a move path. Otherwise the use is `USE_AFTER_MOVE`.

   The diagnostic lists:
   - the move points that reach `P`,
   - whether the move happened in a previous loop iteration, and
   - if the value was only partially moved, the moved field.

   The structured fix is "add `.clone()` at the move point" when the type implements `Clone`, or "borrow instead of moving" (§7.18).

2. Using a whole value (`x`) after a partial move (`x.f` moved) is `USE_AFTER_MOVE`. Using another field (`x.g`) is accepted.

3. A read of the discriminant (from `match` or `if let`) requires the scrutinee to be initialized in the same way.

Because definite initialization is path-based, a move that happens on only one branch makes the place unusable after the join:

```sai
fn branch(s: String, c: Bool) {
    if c {
        consume(s)
    }
    print_len(borrow s)    // rejected: USE_AFTER_MOVE (s may have been moved on the `if` branch)
}

fn in_loop(s: String) {
    var i = 0
    while i < 3 {
        consume(s)         // rejected: USE_AFTER_MOVE (moved in the previous iteration)
        i += 1
    }
}

fn reinit_in_loop() {
    var s = String.new()
    var i = 0
    while i < 3 {
        consume(s)
        s = String.new()   // accepted: s is reinitialized before the next use
        i += 1
    }
}
```

### 7.4.5 Conditional moves and drop flags

At a point where a value would be dropped (§7.13), its move path may be maybe-initialized and maybe-uninitialized at once. In that case the drop is **conditional**: a runtime **drop flag** records whether the value is still owned, and the drop runs only if it is (D-OWN-01). This is the observable rule:

> A value is dropped exactly once if it has not been moved out, at the point given by §7.13. If it has been moved out, it is never dropped by its old owner.

Drop flags are not visible to the program. When the state is statically known, the implementation MUST NOT emit a flag test that changes behavior, and SHOULD emit no flag (see [impl/05](../impl/05-borrow-checker.md) §5.12 and [impl/04](../impl/04-mir.md) for drop elaboration).

If some fields of a struct (one without a `Drop` impl) have been moved out at a drop point, the remaining fields are dropped one by one, in declaration order (§7.13.3), each according to its own state.

---

## 7.5 Borrows

### 7.5.1 Borrow expressions

A borrow is created only by these forms:

- the expressions `borrow p` and `borrow mut p` (03 `PrefixExpr`)
- the auto-ref of a method receiver (§7.5.3)
- a `borrow x` or `borrow mut x` binding in a pattern (06; D-EXPR-04)
- a by-reference closure capture (§7.11)
- the `Index` / `IndexMut` desugaring (§7.2.1 rule 6)
- the `IntoIterator for borrow ..` desugaring of `for x in borrow xs` (D-EXPR-11)

Each evaluation of one of these forms creates exactly one **loan** (§7.9.3).

Typing (the type rules themselves belong to 05):

1. If `Γ ⊢ p place : T`, then `borrow p : borrow 'l T` for a fresh local region `'l` (§7.6.4).
2. If, in addition, `Γ ⊢ p mutable` (§7.2.4), then `borrow mut p : borrow mut 'l T`. If `p` is not mutable, the program is rejected with `BORROW_MUT_OF_IMMUTABLE` (D-OWN-02).
3. The place `p` MUST be initialized (§7.4.4).
4. If the operand is a value expression, it is evaluated into a temporary and the temporary is borrowed. The temporary is dropped at the end of its temporary scope (06), unless §7.5.6 extends it. Because a temporary is mutable (§7.2.4 rule 8), `borrow mut` of a temporary is allowed.

```sai
fn borrows_demo() {
    let a = Vec.new()
    var b = Vec.new()
    touch(borrow mut b)          // accepted
    touch(borrow mut a)          // rejected: BORROW_MUT_OF_IMMUTABLE (fix: change `let a` to `var a`)
}
```

### 7.5.2 Borrows are explicit at call sites

Arguments are never borrowed implicitly (D-OWN-02; R-14). A parameter of type `borrow T` requires an argument of type `borrow T`, which is normally written `borrow e`. The argument can also be an existing borrow value, or one produced by coercion (D-TYPE-04: `borrow mut T` to `borrow T`, `SliceMut<T>` to `Slice<T>`, reborrow).

These mismatches are type errors (05):

| Argument | Parameter | Diagnostic | Structured fix |
|---|---|---|---|
| a place or value of type `T` | `borrow T` | `MISSING_BORROW` | insert `borrow ` before the argument |
| a place of type `T` | `borrow mut T` | `MISSING_BORROW` | insert `borrow mut ` before the argument. If the root is a `let` local, also change `let` to `var` |
| `borrow e` | `borrow mut T` | `MISSING_BORROW` | replace `borrow` with `borrow mut` |
| `borrow e` of type `borrow T` | `T` | `TYPE_MISMATCH` (05) | remove `borrow`, or insert `.clone()` when `T: Clone` |

`MISSING_BORROW` is defined here because it is the language rule D-OWN-02. It is reported by the type checker (05; [impl/03](../impl/03-name-resolution-and-type-checking.md)).

There is no `String` to `Str` coercion (D-TYPE-04). Write `s.as_str()`.

```sai
fn width(image: borrow Image) -> U32 {
    image.width
}

fn caller(image: Image) -> U32 {
    width(image)                 // rejected: MISSING_BORROW (fix: `width(borrow image)`)
}
```

### 7.5.3 Receivers: the only auto-ref

A method call `e.m(args)` resolves `m` through method resolution (05). The method's `self` parameter has one of the types `Self`, `borrow Self`, or `borrow mut Self` (D-TYPE-08). The receiver expression `e` is then adjusted as follows:

1. **Auto-deref.** `e` is dereferenced through `borrow T`, `borrow mut T`, and `Box<T>` as many times as method resolution requires (05 defines the search order). Each step adds a `Deref` projection to the receiver place.
2. **Auto-ref.** With the resulting place `p` of type `Self`:
   - For `self: Self`, `p` is used by value: copied if `Self` is Copy, otherwise moved (§7.3.5 applies, so `r.into_thing()` with `r : borrow T` is `MOVE_OUT_OF_BORROW`).
   - For `self: borrow Self`, the receiver is `borrow p`.
   - For `self: borrow mut Self`, the receiver is a **two-phase** `borrow mut p` (§7.10). `p` MUST be mutable (§7.2.4); otherwise `BORROW_MUT_OF_IMMUTABLE`.
   - If the receiver expression already has exactly the type `self` expects (for example, `r.len()` with `r : borrow Vec<T>`), no auto-ref happens. A `borrow mut` receiver is reborrowed (§7.5.5), not moved.

No other position is ever auto-referenced. In particular, a function called with method syntax through a path (`Vec.push(v, x)`) is an ordinary call, and its arguments follow §7.5.2: `Vec.push(borrow mut v, x)`.

### 7.5.4 Auto-deref of field access

A field access `e.f`, where `e` has type `borrow T`, `borrow mut T`, or `Box<T>`, is the place `(*e).f`. This repeats until a type with field `f` is reached (D-OWN-02). This is the only implicit dereference. Everywhere else a dereference is written with `*`:

```sai
fn read_through(r: borrow I32) -> I32 {
    *r + 1 // explicit `*`; there is no auto-deref for operators
}

fn score(u: borrow User) -> U32 {
    u.score // auto-deref: (*u).score; U32 is Copy, so this is a copy
}
```

A `match` scrutinee of type `borrow T` is not dereferenced automatically. Write `match *b { .. }` (D-EXPR-04). The place `*b` is behind a borrow, so its non-Copy parts can only be bound by `borrow x` (§7.3.5).

### 7.5.5 Reborrowing

A **reborrow** is a borrow of a place that goes through a `Deref` of an existing borrow: `borrow *r`, `borrow mut *r`, or `borrow r.f` with `r : borrow mut T`. The loan is on the place `*r` (or `(*r).f`). The new region `'l` is constrained by `'r : 'l`, where `'r` is the region of `r` (§7.9.4).

Implicit reborrows happen in exactly two cases:

1. **Reborrow coercion** (D-TYPE-04 item 2). At a coercion site (D-TYPE-04), an operand `r` of type `borrow mut 'a T` whose expected type is `borrow mut U` is replaced by `borrow mut *r`. The same holds for `SliceMut<T>`, whose reborrow is a fresh `SliceMut<T>` over the same data. So `r` is not moved, and it can be used again after the new borrow ends (D-OWN-02).
2. **Shared coercion** (D-TYPE-04 items 3 and 4). An operand `r : borrow mut T` whose expected type is `borrow U` becomes `borrow *r`. An operand `s : SliceMut<T>` whose expected type is `Slice<U>` becomes a shared reborrow of the viewed data.

Whether the expected type "is `borrow mut U`" is decided from the expected type **before** generic inference (N-B-5). If the expected type is a bare type parameter `T`, or a type that inference has not yet determined (`let y = r` without an annotation), the operand is moved, not reborrowed:

```sai
fn twice(v: borrow mut Vec<I32>) {
    touch(v)                     // reborrow: v is still usable
    touch(v)                     // accepted
    let w = v                    // move of v (no expected type)
    touch(v)                     // rejected: USE_AFTER_MOVE (fix: `let w = borrow mut *v`)
}
```

A shared view is Copy, so passing one never moves it, and no reborrow is needed.

### 7.5.6 Borrowing temporaries and lifetime extension

The operand of a borrow can be a value expression (§7.2.1). Its temporary lives to the end of the temporary scope that 06 assigns. That is normally the end of the enclosing statement (D-OWN-05). Using the borrow after that point is `TEMPORARY_DROPPED_WHILE_BORROWED`.

**Extension.** If the initializer of a `let` or `var` statement is **syntactically** a borrow expression `borrow e` or `borrow mut e` (after removing parentheses), and `e` is a value expression, then that one temporary lives to the end of the block that encloses the statement (D-OWN-05). The statement may have a type annotation and any pattern. Nothing else is extended (N-B-6). In particular, nothing is extended in these cases:

- a borrow nested inside the initializer (`let p = wrap(borrow make())`, `let r = Holder(item: borrow make())`)
- a borrow in an `if` / `match` branch
- an operand of an assignment
- a borrow produced by auto-ref (`let n = make().len()` is fine, because `n` is a `USize` and holds no borrow)

```sai
fn temps() {
    let s = borrow make_string()            // accepted: the temporary lives to the end of the block
    print_len(s)
    let t = first_word(borrow make_string()) // rejected: TEMPORARY_DROPPED_WHILE_BORROWED
    use_str(t)                               // the later use that needs the temporary
                                             // (fix: insert `let tmp = make_string()` before the statement)
}
```

The structured fix for `TEMPORARY_DROPPED_WHILE_BORROWED` is "bind the temporary to a local first": it inserts `let tmp = <expr>` before the statement and replaces the operand with `tmp` (§7.18).

### 7.5.7 The `'static` region

`'static` is the region that contains every point of every function body and outlives every other region. In v0.1, a value whose type has `'static` in a region position comes from one of these sources (N-B-1):

- a string literal (`Str`, 02 §2.6.4 rule 4) or a byte string literal (`Slice<Byte>`, 02 §2.6.4 rule 5), whose data lives in static data
- an output region of a function with no input regions (§7.7.2 rule O4), or a library function that 16 documents as returning static data
- an unsafe operation in 12 that asserts it

A borrow of a constant item or of any other value expression is a borrow of a temporary (§7.2.1). It is **not** promoted to `'static`, because SAI has no `static` items (D-CAP-02) and no rvalue static promotion. A constant `const GREETING: Str = "hi"` has type `Str` with region `'static`, because its value is a literal.

---

## 7.6 Regions

### 7.6.1 Regions as sets of points

Within one function body, a region is a set whose elements are:

- **points** of the body's CFG (§7.9.1), and
- **end markers** `end('u)`, one for each universal region `'u` of the function (§7.6.4). An end marker stands for "every point in the caller after this function returns, for as long as `'u` lasts there".

`'a : 'b` ("`'a` outlives `'b`") means `'a ⊇ 'b`. Outlives is reflexive and transitive. `'static` contains every point and every end marker, so `'static : 'a` holds for every `'a`.

The region of a borrow is the set of points at which the borrow, or any value derived from it, may still be used (§7.9.4). Regions are never written in source. They appear only in this specification and in diagnostics, where the compiler names them `'1`, `'2`, ... in order of first appearance in the signature and then in the body (§7.18.2).

### 7.6.2 The regions of a type

`regions(T)` is defined by structure. Type arguments are substituted first, and transparent aliases are expanded (05).

| Type | `regions` |
|---|---|
| scalars, `Bool`, `Char`, `Byte`, `Never`, `()` | ∅ |
| `borrow 'a T`, `borrow mut 'a T` | `{'a} ∪ regions(T)` |
| `Str` with region `'a` | `{'a}` |
| `Slice<T>`, `SliceMut<T>` with region `'a` | `{'a} ∪ regions(T)` |
| tuples `(T1, .., Tn)`, `Array<T, N>` | the union of the element types' regions |
| a nominal type `N<T1, .., Tn>` (record, enum, distinct, resource, `Box`, `Vec`, `String`, `Option`, `Result`, ...) | `{'r}` if `N` has an implicit region parameter instantiated with `'r` (§7.8.2), united with `regions(T1) ∪ .. ∪ regions(Tn)` |
| `Ptr<T>`, `PtrMut<T>` | `regions(T)`. A raw pointer has no region of its own (§7.17) |
| function pointer types | ∅. All their regions are bound inside the type (§7.7.6, N-B-7) |
| a function item type | the regions of its generic arguments |
| a closure type | the regions of its captured places' types and of its by-reference captures (§7.11.4) |
| a type parameter `T`, an associated type projection `T.Item` | opaque (§7.6.5) |
| the future of an `async fn` or `async` block (Stage 5) | as defined in 11. It includes the regions of every captured parameter |

A type **carries a region** if `regions(T) ≠ ∅` after substitution. "Ignoring type parameters" (D-OWN-03) means that, in a generic definition, an opaque `T` contributes no region of its own. Its regions belong to the type argument.

### 7.6.3 Subtyping and variance

The only subtyping in SAI is region subtyping (D-TYPE-04 item 8). `S <: T` holds if `S` and `T` are equal except for regions, and every region position is related according to its variance:

- In a **covariant** position, `'s : 't` is required.
- In a **contravariant** position, `'t : 's` is required.
- In an **invariant** position, both are required.

Variance is fixed as follows:

| Type | Region | Type argument |
|---|---|---|
| `borrow 'a T` | covariant in `'a` | covariant in `T` |
| `borrow mut 'a T` | covariant in `'a` | invariant in `T` |
| `Str` | covariant | — |
| `Slice<T>` | covariant | covariant in `T` |
| `SliceMut<T>` | covariant | invariant in `T` |
| `Ptr<T>` | — | covariant in `T` |
| `PtrMut<T>` | — | invariant in `T` |
| tuples, `Array<T, N>` | — | covariant in each element |
| function pointer types | — (regions are bound, so there is no free position, §7.7.6) | contravariant in each parameter type, covariant in the return type |

For a user-defined nominal type, the variance of its implicit region and of each type parameter is computed from its fields (and from all variants' fields for an enum):

- It is the greatest lower bound of the variances of all its occurrences. An occurrence's variance is the composition of the variances along the path from the field type down to the occurrence.
- Invariant absorbs everything. Covariant composed with covariant is covariant.
- Recursive and mutually recursive types are solved as a least fixed point that starts from "unused".
- A parameter that ends up unused is treated as invariant (N-B-8).
- Core and alloc types (`Box`, `Vec`, `Option`, `Result`, ...) use the same rule on their definitions. A core type with interior mutability is invariant in its parameter, as 16 declares.

Region subtyping is applied at **every value flow**: every copy or move of a value into a place, every argument, every return, and every field or element of an aggregate. It is not limited to the coercion sites listed in D-TYPE-04 (N-B-9). Because regions have no run-time representation, this choice never changes behavior. It only makes the region solution as small as possible.

### 7.6.4 Universal and local regions

The regions of a function body are of two kinds:

- **Universal regions** are fixed by the signature. They are:
  - `'static`
  - one region for each input region position (§7.7.1)
  - the output region `'o` (§7.7.2)
  - the regions of the enclosing impl's target type (§7.7.5)

  Every universal region `'u` contains every point of the body and its own end marker `end('u)`.
- **Local regions** are all other regions. They are:
  - the region of each borrow expression and loan
  - each region position in the types of locals and temporaries
  - the instantiation of a callee's universal regions at each call (§7.9.4)

  Local regions are inferred as the least solution of the constraints of §7.9.4.

The signature **assumes** a set of outlives relations between universal regions:

- `'static : 'u` for every `'u`
- reflexivity
- the implied bounds of §7.6.5
- the output relations `'x : 'o` from §7.7.2

The function is only allowed to rely on the transitive closure of these assumptions. If the solution puts `end('v)` into a universal region `'u` for which `'u : 'v` is not assumed, the body needs more than the signature promises, and the function is rejected (§7.9.7).

### 7.6.5 Well-formedness and implied bounds

A type is **well-formed** (WF) only if the following hold:

- `borrow 'a T`, `borrow mut 'a T`, `Slice<T>` / `SliceMut<T>` with region `'a`: every region of `T` outlives `'a`, and every type parameter `P` that occurs in `T` satisfies `P : 'a` ("all of `P`'s regions outlive `'a`").
- A nominal type with region `'r` and arguments `T1..Tn`: the WF conditions of its field types, with `'r` and `T1..Tn` substituted.

Every type that is written, or produced by elision, in the signature is assumed WF inside the body. These **implied bounds** become assumptions (§7.6.4). For example, a parameter `p: borrow Parser`, elided to `borrow '1 Parser<'2>`, implies `'2 : '1`. The caller establishes WF for the argument types: it constructs only WF types, because every borrow it creates is checked at its creation point.

Inside a generic body, every region of a type argument is a region of the caller. It therefore contains every point of the body, and the end marker of every universal region `'u` for which `P : 'u` is implied. An obligation `P : 'l` on a type parameter `P` (a **type test**) holds if and only if, in the region solution, every end marker `end('u)` in `'l` satisfies `P : 'u` as an implied bound of the signature. Points of the body never make a type test fail. So `borrow x` with `x : P` is always valid inside the body, and returning a `borrow P` requires `P : 'o` to be implied. A projection `P.A` is treated like a type parameter, and `P.A : 'l` also holds whenever `P : 'l` holds: by N-B-12, the regions of an associated type are regions of the impl's target type or `'static`.

Otherwise the obligation fails, and the function is rejected with `BORROW_ESCAPES_SIGNATURE` (§7.9.7). A generic body is checked once, on generic MIR (D-MIR-01). It is not checked again after monomorphization (§7.19).

---

## 7.7 Regions in signatures

A function's borrowing contract is determined by its signature alone: its parameter types, its return type, and its `borrows` clause (D-OWN-03; C-05). This section gives the exact elision procedure. It runs on the signature after name resolution and alias expansion. It never looks at the body.

### 7.7.1 Input regions

1. **I1.** Each **view position** in a parameter type (including the type of `self`) gets its own fresh universal region. A view position is each occurrence of `borrow`, `borrow mut`, `Str`, `Slice`, or `SliceMut` in the type, after transparent aliases are expanded.
2. **I2.** Each occurrence of a nominal type that has an implicit region parameter (§7.8.2) gets its own fresh universal region, which instantiates that parameter.
3. **I3.** Positions inside a function pointer type or an `Fn` / `FnMut` / `FnOnce` bound are bound by that type (§7.7.6). They are not regions of the function.
4. **I4.** A type parameter contributes no position.

The **input regions** are the regions created by I1 and I2. They are numbered `'1`, `'2`, ... in left-to-right order of the parameters, and within a type in pre-order.

### 7.7.2 The output region

1. **O1.** All region positions in the return type get one and the same universal region `'o` (N-B-10). These are its view positions and its occurrences of region-carrying nominal types, outside function pointer types. If the return type has no such position, there is no output region, and O2–O4 do not apply.
2. **O2 (rule (a)).** If the function has a `borrows { π1, .., πk }` clause, then for every `i` and every region `'x ∈ regions(type(πi))`, the signature assumes `'x : 'o`. Here `type(πi)` is computed on the elided parameter types (§7.7.3).
3. **O3 (rule (b)).** Otherwise, if the function has a `self` parameter and `regions(type(self)) ≠ ∅`, the signature assumes `'x : 'o` for every `'x ∈ regions(type(self))`.
4. **O3′ (rule (c)).** Otherwise, the signature assumes `'x : 'o` for every input region `'x`.
5. **O4.** If the set of regions that outlive `'o` by O3′ is empty, because the function has no input region at all, then `'o` is `'static` (N-B-11). Under O2 and O3 the set is never empty (§7.7.3 V3, and the condition of O3).

So the result may borrow from exactly the sources the signature names, and the caller keeps exactly those sources borrowed while the result is in use (§7.9.4).

### 7.7.3 The `borrows` clause: syntax and validity

The syntax is `BorrowsClause` of 03 §3.5. It appears after the `where` clause and before the `effects` clause (D-SYN-06):

```sai
fn longest(a: Str, b: Str, cfg: borrow Config) -> Str
    borrows { a, b }
{
    if a.len() >= b.len() {
        a
    } else {
        b
    }
}
```

Each provenance `π = root.f1. .. .fn` is checked as follows:

1. **V1.** `root` MUST be the name of a parameter of this function, or `self`.
2. **V2.** Each `.fi` MUST name a field of the type reached so far. Auto-deref through `borrow`, `borrow mut`, and `Box` applies, exactly as for field access (§7.5.4). The field MUST be accessible at the function's definition site (04). For a distinct type, `.value` is the only field (D-TYPE-03). A path cannot go through a type parameter, an enum, a tuple, or a raw pointer.
3. **V3.** `regions(type(π))`, computed on the elided parameter types, MUST be non-empty.
4. **V4.** The return type MUST have an output region (§7.7.2 O1).

A violation of V1–V3 is `BORROWS_INVALID_PROVENANCE`. A violation of V4 is `BORROWS_WITHOUT_OUTPUT_REGION`, with the structured fix "remove the `borrows` clause". A provenance that is equal to another one, or that has another one as a strict prefix, adds nothing, because `regions(p.f) ⊆ regions(p)`. It is the warning `REDUNDANT_PROVENANCE`, with the fix "remove it".

A field path selects regions by **type**, not by location. With `self: borrow mut Self` and `Self = Parser` with region `'p`, the type of `self` is `borrow mut '1 Parser<'p>`. Then `self` names `{'1, 'p}`, and `self.input` (of type `Str` with region `'p`) names `{'p}` only. Because a user type has a single region (§7.8.2), every region-carrying field of the same value names the same region. A field path is useful to exclude the outer borrow (`self.input` instead of `self`), or to select the regions of one type argument (`w.items` in `Wrapper<T>`).

### 7.7.4 Provenance is signature-only

Provenance is never inferred from the body (D-OWN-03). The body is checked against the signature (§7.9.7). Changing a body can therefore never change whether a caller is accepted, and the borrowing contract is visible in the API and in semantic diffs (P8).

If the body returns a value whose regions are not allowed by the signature, the function is rejected:

- If the value borrows from a local, a temporary, or a by-value parameter (whose storage ends when the function returns), the error is `RETURNS_LOCAL_BORROW`.
- If the value borrows from a parameter's data that the signature does not list, the error is `BORROWS_CONTRACT_VIOLATED`. The structured fix adds that provenance to the `borrows` clause, or creates the clause if there is none. When there is no `borrows` clause and rule (b) applies, the fix is "add `borrows { self, p }`". When the missing source is a parameter under rule (c), no fix is needed, because every input is already a source.

```sai
type Parser {
    input: Str
    pos: USize
}

impl Parser {
    fn rest(self: borrow mut Self) -> Str
        borrows { self.input }
    {
        self.pos = self.input.len()
        self.input
    }

    fn rest_tied(self: borrow mut Self) -> Str {    // rule (b): the result keeps `self` mutably borrowed
        self.pos = self.input.len()
        self.input
    }
}

fn two_tokens(text: String) {
    var p = Parser(input: text.as_str(), pos: 0)
    let a = p.rest()
    let b = p.rest()             // accepted: `a` borrows `text`, not `p`
    use_str(a)
    use_str(b)
}

fn two_tokens_tied(text: String) {
    var p = Parser(input: text.as_str(), pos: 0)
    let a = p.rest_tied()
    let b = p.rest_tied()        // rejected: CONFLICTING_BORROW (the result `a` keeps `p` mutably borrowed)
    use_str(a)
    use_str(b)
}

fn pick(a: Str, b: Str) -> Str
    borrows { a }
{
    b                            // rejected: BORROWS_CONTRACT_VIOLATED (fix: `borrows { a, b }`)
}

fn dangling() -> borrow String {
    let s = String.new()
    borrow s                     // rejected: RETURNS_LOCAL_BORROW
}
```

### 7.7.5 Impls and traits

1. **Impl regions.** The target type of an `impl` is elided like a parameter type (I1–I4). Its regions are universal regions of every method in the impl. `Self` denotes the target type with those regions. In `impl Parser`, `Self = Parser<'p>`, and every method sees the same `'p`.
2. **Associated types and trait arguments.** In a trait impl, the region positions in associated type definitions and in the trait's arguments are bound as follows (N-B-12):
   - If the target type carries exactly one region, they denote that region.
   - If it carries no region, they denote `'static`.
   - If it carries more than one region (for example `impl Iterator for borrow Parser`), a region position in these places is `AMBIGUOUS_IMPL_REGION`. Wrap the borrow in a record type, which has a single region.

   ```sai
   type Words {
       rest: Str
   }

   impl Iterator for Words {
       type Item = Str // Str with the region of Words
       fn next(self: borrow mut Self) -> Option<Self.Item> {
           if self.rest.is_empty() {
               None
           } else {
               let w = self.rest
               self.rest = ""
               Some(w)
           }
       }
   }
   ```

   In the trait, the return type `Option<Self.Item>` is a projection with no elided position, so `next` has no output region. In the impl it is `Option<Str>` with the region of `Words`. The items therefore borrow the text, not the iterator, and they stay usable while iteration continues.

   An impl method's own signature is elided by the ordinary rules. If the impl above wrote `-> Option<Str>` instead of `-> Option<Self.Item>`, O1 would give the `Str` the output region `'o`, and rule (b) would tie it to the borrow of `self`. That signature is weaker than the trait's, so it is rejected by item 4 with `IMPL_SIGNATURE_REGION_MISMATCH`. The structured fix replaces the written type with `Self.Item`.
3. **Trait method signatures** are elided like functions. In a trait, `Self` is opaque (§7.6.5). For `self: borrow Self`, rule (b) uses `regions(borrow '1 Self)`, which is `'1` together with the opaque regions of `Self`.
4. **Compatibility.** Elide both signatures, then substitute `Self` and the associated types into the trait method's signature. The impl method's signature MUST then be identical to it, including regions (N-B-13):
   - the parameter types, with input regions identified position by position;
   - the return type;
   - the transitive closure of the outlives assumptions on the output region.

   A difference in the region-erased types is a type error, which belongs to 05. A difference only in regions is `IMPL_SIGNATURE_REGION_MISMATCH`. The structured fix rewrites the impl method's return type and `borrows` clause to match the trait's.

### 7.7.6 Function pointer types and `Fn` bounds

A function pointer type `fn(A1, .., An) -> R effects {..}` (D-TYPE-09) and a bound written with `Fn(A1, .., An) -> R`, `FnMut(..)`, or `FnOnce(..)` sugar are elided **locally**, as if they were the signature of a function with no `self` and no `borrows` clause (N-B-7):

- Each view position and region-carrying nominal occurrence in `A1..An` gets a region **bound by the type**. The type is polymorphic in that region: "for every `'a`".
- The positions in `R` get one region, which is outlived by every bound input region of that type, or is `'static` if there is none (O3′, O4).
- A function pointer type therefore carries no free region (§7.6.2), and it does not make a containing type region-carrying (§7.8.2).

A function item coerces to the pointer type (D-TYPE-09) only if its elided signature, instantiated with its generic arguments, is identical to the pointer type's up to renaming of bound regions: it has no free region (no region from a generic argument or from the impl), its input regions correspond position by position to the bound input regions, and the relations `'x : 'o` it assumes are exactly those that the pointer type assumes. Otherwise the coercion is `FN_POINTER_REGION_MISMATCH`. A closure never coerces to a function pointer type (05 N-A-31). A closure checked against an `Fn`-family bound takes the bound's signature as its own (§7.11.1).

```sai
fn apply(f: fn(Str) -> Str, s: Str) -> Str { // f: for every 'a, fn(Str<'a>) -> Str<'a>
    f(s) // the result borrows `s`, by rule (c) of f's type
}

fn trim_start(s: Str) -> Str {
    s
}

fn use_apply() -> Str {
    apply(trim_start, "  x") // accepted: the result has region 'static
}
```

### 7.7.7 Elision examples

In the internal form, `Parser<'p>` means the implicit region of `Parser` instantiated with `'p`. `Str<'a>` means `Str` with region `'a`.

| Written signature | Internal form and assumptions |
|---|---|
| `fn len(s: Str) -> USize` | `fn len(s: Str<'1>) -> USize`; no output region |
| `fn first(v: borrow Vec<I32>) -> borrow I32` | `fn first(v: borrow '1 Vec<I32>) -> borrow 'o I32`; `'1 : 'o` |
| `fn longest(a: Str, b: Str) -> Str` | `Str<'1>, Str<'2> -> Str<'o>`; `'1 : 'o`, `'2 : 'o` |
| `fn longest(a: Str, b: Str) -> Str borrows { a }` | as above, but only `'1 : 'o` |
| `fn get(self: borrow Parser, key: Str) -> Str` | `self: borrow '1 Parser<'2>, key: Str<'3> -> Str<'o>`; rule (b): `'1 : 'o`, `'2 : 'o`; implied `'2 : '1` |
| `fn get(self: borrow Parser, key: Str) -> Str borrows { self.input }` | as above, but only `'2 : 'o` |
| `fn split(s: Str) -> (Str, Str)` | `Str<'1> -> (Str<'o>, Str<'o>)`; `'1 : 'o` |
| `fn name() -> Str` | `-> Str<'static>` (O4) |
| `fn wrap<T>(x: borrow T) -> borrow T` | `borrow '1 T -> borrow 'o T`; `'1 : 'o`; implied `T : '1` |
| `fn keep<T>(x: T) -> T` | no regions; the regions of `T` belong to the caller's argument |
| `fn each(v: borrow Vec<Str>, f: fn(Str) -> Bool)` | `v: borrow '1 Vec<Str<'2>>`, implied `'2 : '1`; `f`'s region is bound by its own type |

---

## 7.8 View types and types that contain borrows

### 7.8.1 View types

Each view type carries exactly one region of its own (D-TYPE-02):

- `Str` is the value `(ptr, len)` over UTF-8 data in that region.
- `Slice<T>` and `SliceMut<T>` are `(ptr, len)` over elements of `T`.
- `borrow T` and `borrow mut T` are pointers.

The shared views `Str`, `Slice<T>`, and `borrow T` are Copy. A copy is another borrow of the same data with the same region. Any number of copies may exist, and none of them permits writing.

The exclusive views `SliceMut<T>` and `borrow mut T` are not Copy. At each point, at most one usable path to the viewed data exists. Using the view moves it, unless it is reborrowed (§7.5.5).

### 7.8.2 Records and enums that contain borrows

A user-defined record type, enum, or distinct type `N` **has an implicit region parameter** if any of its field types (or variant field types), after alias expansion, syntactically contains one of these, outside function pointer types and `Fn`-family bounds:

- a view type, or
- a nominal type that has an implicit region parameter.

Type parameters of `N` are ignored (D-OWN-03). For recursive and mutually recursive types, the property is the least fixed point. Start by assuming no type has a region parameter, and iterate until nothing changes.

If `N` has an implicit region parameter `'r`, then:

1. `N` has **exactly one** region parameter (D-OWN-03). Every view position in every field is `'r`. Every region-carrying nominal type in a field is instantiated with `'r`.
2. Regions that arrive through a type argument belong to that argument: `Vec<Str>` is `Vec<Str<'s>>`, and `Vec` has no region parameter.
3. Constructing `N(f1: e1, ..)` requires every field value to be a subtype of its field type with `'r` (§7.6.3). So every borrow stored in the value MUST outlive `'r`, and `'r` is at most the intersection of the stored borrows' regions.
4. A value of type `N<'r>` is usable only at points in `'r`. Using a field moved or copied out gives a value whose region is `'r`. The region of an individual source borrow is not recovered.
5. `N` is Copy only if Copy is derived (D-TYPE-08), even if all its fields are shared views.

A consequence of rule 3: a record that holds two borrows ties both to the same region. A value cannot outlive the shorter of the two borrows, even if only the longer one is used later. When that is too restrictive, split the record into two values, or store owned data (P9).

```sai
type Span {
    text: Str
    note: Str
}

fn make_span(doc: borrow String) -> Str {
    let tmp = "note".to_string()
    let s = Span(text: doc.as_str(), note: tmp.as_str())   // the loan of `tmp` is created here
    s.text                       // rejected: RETURNS_LOCAL_BORROW (`s` ties the borrow of `tmp` to the result)
}

enum Token {
    Word(Str)
    Number(I64)
}

fn first_token(src: Str) -> Token {      // Token has a region; rule (c): it borrows `src`
    Token.Word(src)
}
```

In `make_span`, `s.text` does not hold the loan of `tmp`, but its type carries the single region of `s`. The result must live in `'o`, which requires the region of `s` to outlive `'o`, which requires `tmp` to be borrowed until after the function returns. `tmp` is dropped at the end of the body while that loan is still live, and the loan's region must outlive the output region, so the code is `RETURNS_LOCAL_BORROW` (§7.9.6). Its explanation labels `tmp.as_str()` as the creation of the loan and the drop of `tmp` as the conflict.

### 7.8.3 Aliases and distinct types

- A transparent alias `type Name = Str` has no region parameter of its own. Each use of `Name` is a view position, exactly as if `Str` were written.
- A distinct type `distinct Name = Str` is nominal. It has an implicit region parameter by the rule of §7.8.2, because its base type contains a view. Its single field is `.value` (D-TYPE-03).
- A refined type `type Short = Str where ..` is nominal (D-TYPE-03) and follows the same rule as a distinct type.

### 7.8.4 Resources

A `resource` (D-OWN-06) MUST NOT have an implicit region parameter. A field type of a resource that contains a view type or a region-carrying type is `RESOURCE_CONTAINS_BORROW` (N-B-14). A resource maps one-to-one to a WIT resource (D-OWN-06). A WIT handle can outlive any borrow of the component's own memory. A resource can hold owned data (`String`, `Vec<T>`, `Box<T>`, other resources) and indices.

### 7.8.5 Borrows at component boundaries

A non-resource return type that contains a borrow cannot cross a component boundary (D-WIT-01). The rules for `export` and import signatures, including how WIT `borrow<r>` parameters map to `borrow R`, belong to 13. For the checker, an imported function is an ordinary function whose signature is elided by §7.7.

---

## 7.9 The borrow rules

This section defines declaratively which borrows are valid (D-OWN-04). The definition is non-lexical: a borrow lasts as long as it, or a value derived from it, may still be used, not to the end of a lexical scope.

### 7.9.1 The control-flow model

A function body is a CFG of basic blocks. Each block is a sequence of statements followed by one terminator, as in MIR (D-MIR-01; [impl/04](../impl/04-mir.md)). A **point** is the position of one statement or one terminator. The successors of a statement are the next position in its block. The successors of a terminator are the first positions of its target blocks.

The construction of the CFG from source follows 06. The checker relies on these properties of it:

1. Evaluation order is the one of D-EXPR-02 (left to right, receiver first, the RHS of an assignment before its place).
2. Each evaluation of a borrow form (§7.5.1) is one point that creates one loan.
3. At the end of each scope, the drops of §7.13 appear in the order of §7.13.2, each followed by the end of the local's storage (`StorageDead`).
4. `return`, `break`, `continue`, and `?` pass through the drops and storage ends of every scope they exit.
5. A trap (D-ERR-01: panics, failed checks, `unreachable()`) is a terminator with no successors. No drop runs after it. So nothing after a trap can conflict with anything.
6. Every `.await` is a single terminator that takes the future by move and writes its result (Stage 5, §7.16).

### 7.9.2 Accesses

Each point performs zero or more **accesses**. An access has a place, a kind, and a depth.

| Kind | Performed by | Depth |
|---|---|---|
| **read** | a copy of `p`; the operand of a shared `borrow p`; a reserved two-phase borrow (§7.10) | deep |
| **read** | a read of the discriminant or length of `p`; the scrutinee "fake read" of `match`, `if let`, and `let .. else` | shallow |
| **write** | `borrow mut p`; the activation of a two-phase borrow (§7.10) | deep |
| **write** | an assignment to `p` (including a call's destination and a `SetDiscriminant`) | shallow |
| **move** | a move of `p` (§7.3.2) | deep |
| **drop** | the drop of `p` (§7.13), if `type(p)` has drop glue | drop (below) |
| **storage-dead** | the end of the storage of local `x` | shallow |

An access to place `p` is **relevant** to a loan of place `q` in these cases:

- **Deep access:** `p` and `q` overlap (§7.2.3).
- **Shallow access:** either `q` is a prefix of `p`, or `q` extends `p` with projections none of which is a `Deref`. A shallow access to `p` does not touch data that is only reachable through a pointer stored in `p`.
- **Drop access:** `p` and `q` overlap, the path of `q` has no `Deref` of a shared view (`borrow T`, `Str`, `Slice<T>`), and, if `q` extends `p`, the projections of `q` after `p`, read from left to right, select a field of a value whose type has a `Drop` impl or is a resource before they reach a `Deref` of a view type or of a raw pointer, or they reach no such `Deref` at all. A drop reaches data behind a `borrow mut` stored in the value only through a `Drop` impl. A `Deref` of `Box<T>` does not stop it.

Treating drop as a write-like access that conflicts with shared loans, and that reaches through a stored borrow only by way of a `Drop` impl, is a decision of this chapter (N-B-15). D-OWN-04 does not list drop explicitly. Without this rule, dropping a value would invalidate the data that a live shared borrow points into.

### 7.9.3 Loans

A **loan** is a tuple `L = (q, k, 'l, P)`:

- `q` is the borrowed place.
- `k` is the kind: `shared`, `mut`, or `two-phase mut` (§7.10).
- `'l` is the borrow's region.
- `P` is the point that creates it.

Each borrow form (§7.5.1) creates one loan each time it is evaluated. A loan of a place behind a shared view is recorded too. Since such a place is never mutable, no access can conflict with it.

**Reborrow constraints.** For a loan of `q = x.π` with region `'l`, walk `π` from its last projection back toward `x`, and look at each `Deref` projection:

- At a `Deref` of `borrow mut 'a T` (or of a `SliceMut` element with region `'a`), add `'a : 'l` and continue.
- At a `Deref` of `borrow 'a T` (or of a `Str` or `Slice` element with region `'a`), add `'a : 'l` and stop.
- At a `Deref` of `Box<T>`, continue.
- At a `Deref` of a raw pointer, stop. Raw pointers are outside the checker (§7.17).

**WF of the borrow.** The type `borrow 'l T` MUST be WF: every region of `T` outlives `'l` (§7.6.5).

### 7.9.4 Liveness and the region solution

**Liveness of locals.**

- A local `x` is **use-live** at point `P` if some path from `P` reaches a use of `x` before it reaches an assignment to the whole of `x` or `StorageDead(x)`. A use of `x` is any access to a place rooted at `x` other than an assignment to the whole of `x`, a drop, and `StorageDead`.
- A local `x` is **drop-live** at `P` if some path from `P` reaches a drop of `x` (or of a place rooted at `x`) before it reaches an assignment to the whole of `x` or `StorageDead(x)`.

**Liveness constraints.** For every point `P` and local `x`:

- If `x` is use-live at `P`, every region in `regions(type(x))` contains `P`.
- If `x` is drop-live at `P`, every region in `droplive(type(x))` contains `P`. `droplive` is defined in §7.13.7.

**Outlives constraints.** Each point contributes the following:

1. **Value flow.** For every assignment of a value of type `S` to a place of type `T`, and for every operand passed or returned, `S <: T` (§7.6.3, N-B-9). The relation produces the outlives constraints between the region positions of `S` and `T`.
2. **Borrows.** For a borrow of `q` with region `'l` assigned to a place of type `T`: `borrow 'l type(q) <: T`, plus the reborrow constraints and the WF constraints of §7.9.3.
3. **Calls.** At a call of a function `f`:
   - Each universal region of `f` other than `'static` is instantiated with a fresh local region variable. This includes the regions of the impl, if `f` is a method.
   - Each assumption `'x : 'o` of `f` (§7.7.2) becomes a constraint between the fresh variables.
   - The WF conditions of `f`'s instantiated parameter types and return type become constraints.
   - Each argument's type must be a subtype of its instantiated parameter type, and the instantiated return type must be a subtype of the destination's type.
4. **Aggregates.** Each field or element value of an aggregate is a value flow into the instantiated field type.
5. **Universal regions.** Every universal region contains every point and its own end marker. Every assumption `'u : 'v` of the signature adds `end('v)` to `'u`.
6. **Return.** The value in the return place is a value flow into the declared return type, with its universal regions.

**The solution.** The region solution is the least assignment of sets to region variables that satisfies all liveness constraints (`P ∈ 'r`) and all outlives constraints (`'a ⊇ 'b`). Outlives constraints are location-insensitive (N-B-24): `'a : 'b` requires `'a` to contain all of `'b`, wherever the constraint was generated. The least solution exists and is unique, because all constraints are monotone set inclusions over a finite domain. The reference algorithm that computes it is in [impl/05](../impl/05-borrow-checker.md) §5.7.

At a call, the effect of item 3 is this. The output region of the call's result contains every point where the result is live. Through `'x : 'o`, so does the region of every source argument, and therefore so does the region of the loan behind that argument. This is exactly the rule that "inputs stay borrowed while the result is in use" (D-OWN-03 rule (c); D-OWN-04).

### 7.9.5 Loans in scope

A loan `L = (q, k, 'l, P)` is **in scope** at point `Q` if there is a non-empty CFG path from `P` to `Q` that meets two conditions:

- Every point on the path after `P`, up to and including `Q`, is in `'l`.
- No point strictly between `P` and `Q` **kills** `L`.

An assignment to the whole of a local `x` kills every loan whose place is rooted at `x`. So does `StorageDead(x)`. After such a point, the place `q` no longer denotes the memory that was borrowed. A conflicting access to `q` itself at that point is detected first, because the killing point is checked against the loans that are in scope there.

The loan's own creation point is not in its scope. So a borrow never conflicts with itself.

### 7.9.6 Conflicts

**Rule.** The program is rejected if there are a point `Q`, an access `A` at `Q` to a place `p`, and a loan `L` of a place `q` in scope at `Q`, such that `A` is relevant to `L` (§7.9.2) and the following table marks the pair as a conflict.

| Access kind | Shared loan | Mutable loan |
|---|---|---|
| read (deep or shallow) | — | conflict |
| write (assignment, `borrow mut`, two-phase activation) | conflict | conflict |
| move | conflict | conflict |
| drop | conflict (N-B-15) | conflict |
| storage-dead | conflict | conflict |

A two-phase loan that has not been activated yet counts as a shared loan (§7.10).

Accesses made **through** a borrow are accesses to places rooted at the local that holds the borrow (for example `*r`). They are not accesses to `q`. So using a borrow never conflicts with its own loan.

**Diagnostic code.** The code is chosen by the access. The first row that applies is used:

| Access | Code |
|---|---|
| a shared or mutable borrow, including a two-phase activation and a by-borrow closure capture | `CONFLICTING_BORROW` |
| a read (copy, discriminant read) | `USE_WHILE_MUT_BORROWED` |
| an assignment, or the drop of the old value that an assignment performs | `ASSIGN_WHILE_BORROWED` |
| a move, including a by-move capture and `core.mem.drop(x)` | `MOVE_WHILE_BORROWED` |
| a drop or storage-dead of the arena of a `region` block (§7.15) | `BORROW_ESCAPES_ARENA` |
| a drop or storage-dead of a local, when the loan flows into the return value: an outlives chain leads from the loan's region to a region of the return place or to the output region `'o` | `RETURNS_LOCAL_BORROW` |
| a drop or storage-dead of a temporary | `TEMPORARY_DROPPED_WHILE_BORROWED` |
| any other drop or storage-dead of a local | `DOES_NOT_LIVE_LONG_ENOUGH` |

Each of these diagnostics carries the three-point explanation of §7.18.3.

### 7.9.7 Conformance to the signature

After the solution is computed, the following checks are made:

1. For every universal region `'u` and every end marker `end('v)` in `'u`, the relation `'u : 'v` MUST follow from the signature's assumptions (§7.6.4). If it does not:
   - If `'v` is the output region `'o`, the body returns a borrow that the signature does not allow. The code is `BORROWS_CONTRACT_VIOLATED` (§7.7.4).
   - Otherwise, the body stores a borrow with one input region into a place whose type has another input region, for example by pushing a parameter `s: Str` into a parameter `v: borrow mut Vec<Str>`. The code is `BORROW_ESCAPES_SIGNATURE`.
2. Every type test `P : 'l` (§7.6.5) MUST hold. Otherwise the code is `BORROW_ESCAPES_SIGNATURE`.

Item 1 has a deliberate consequence. Region parameters cannot be written, and `borrows` constrains only the output (D-OWN-03). So a function cannot store a borrow from one parameter into data reached through another parameter. Such code is restructured: put both borrows into one record (which has a single region, §7.8.2), make the function a method on a type that holds the target, or return the borrow to the caller (D-OWN-04, "restructure").

### 7.9.8 Examples

```sai
fn nll_ends_at_last_use() {
    var v = Vec.new()
    v.push(1)
    let first = borrow v[0]      // shared loan of v (through Index)
    let n = *first + 1           // last use of `first`
    v.push(n)                    // accepted: the shared loan is no longer live
}

fn conflict() {
    var v = Vec.new()
    v.push(1)
    let first = borrow v[0]
    v.push(2)                    // rejected: CONFLICTING_BORROW (the receiver of `push` borrows v mutably)
    let n = *first               // the later use that keeps `first` live
}

fn read_while_mut() {
    var x = 1
    let r = borrow mut x
    let y = x                    // rejected: USE_WHILE_MUT_BORROWED
    *r = 2
}

fn assign_while_shared() {
    var x = 1
    let r = borrow x
    x = 2                        // rejected: ASSIGN_WHILE_BORROWED
    let y = *r
}

fn disjoint_fields(p: borrow mut Pair) {
    let a = borrow mut p.left
    let b = borrow mut p.right   // accepted: (*p).left and (*p).right are disjoint
    a.push_str("x")
    b.push_str("y")
}

fn overlapping_indices() {
    var arr: Array<I32, 4> = Array.filled(0)
    let a = borrow mut arr[0]
    let b = borrow mut arr[1]    // rejected: CONFLICTING_BORROW (indices are never compared, §7.1.3)
    *a = *b
}

fn does_not_live_long_enough() {
    let outer = String.new()
    var r = borrow outer
    {
        let inner = String.new()
        r = borrow inner         // rejected: DOES_NOT_LIVE_LONG_ENOUGH (`inner` is dropped at the end of the block)
    }
    print_len(r)
}

fn reborrow_chain(v: borrow mut Vec<I32>) {
    let w = borrow mut *v        // reborrow: loan of *v
    w.push(1)
    v.push(2)                    // accepted: `w` is no longer live
}
```

For `overlapping_indices`, the remedy is a library operation that takes both indices under one borrow, such as a swap, or one that splits a `SliceMut` into two disjoint halves (16).

---

## 7.10 Two-phase borrows

Two-phase borrows apply **only** to auto-referenced `borrow mut` receivers (D-OWN-04). These are the cases:

1. The receiver of a method call whose `self` type is `borrow mut Self`, when the receiver is auto-referenced (§7.5.3 rule 2). An existing `borrow mut` receiver that is reborrowed by rule 2's last item is also two-phase, because the reborrow is created implicitly.
2. The base `e` of an `IndexMut` place `e[i]` (§7.2.1 rule 6). The desugaring auto-references `e` as the receiver of `index_mut` (N-B-16).

They never apply to a `borrow mut` written in source, to `borrow mut` pattern bindings, or to closure captures.

A two-phase loan `L` of place `q` is created at its **reservation point** `R`, which is the point where the receiver is evaluated (receiver first, D-EXPR-02). It is **activated** at the point `A` of the call (or of the `index_mut` call):

- **From `R` up to `A`**, `L` behaves as a shared loan. The arguments may read `q`, copy from it, and borrow it shared. Writes, moves, mutable borrows, and drops of `q` conflict with `L` by the shared column of §7.9.6.
- **At `A`**, the activation is a deep **write** access to `q`. It is checked against every other loan in scope at `A`. So every shared loan that an argument created MUST be dead at `A`.
- **After `A`**, `L` is an ordinary mutable loan.

```sai
fn two_phase(v: borrow mut Vec<USize>) {
    v.push(v.len())                    // accepted: `len` only reads *v before `push` activates
}

fn take_last(v: borrow mut Vec<USize>) -> USize {
    v.len()
}

fn not_two_phase() {
    var v: Vec<USize> = Vec.new()
    v.push(take_last(borrow mut v))    // rejected: CONFLICTING_BORROW (a `borrow mut` inside the reservation)
    Vec.push(borrow mut v, v.len())    // rejected: CONFLICTING_BORROW (an explicit `borrow mut` is not two-phase)
}

fn index_two_phase(v: borrow mut Vec<USize>) {
    (*v)[v.len() - 1] = 0              // accepted: the base of `index_mut` is a two-phase borrow (N-B-16)
}
```

The structured fix for the explicit case is "evaluate the argument first": it inserts `let tmp = <arg>` before the statement and replaces the argument with `tmp`.

---

## 7.11 Closures and captures

### 7.11.1 Closure signatures

A closure `fn(params) -> R { body }` (D-TYPE-09; 03 `ClosureExpr`) has parameter and return types inferred by 05. Regions in them are set as follows (N-B-17):

- If the closure is checked against an expected `Fn` / `FnMut` / `FnOnce` bound, it takes that signature, with its bound regions (§7.7.6).
- Otherwise, every region position in its parameter and return types is a local region variable of the enclosing body. These are not elided independently per call.

A region bound by the expected signature stands for any region that a caller of the closure may choose. Inside the closure body it is a universal region of the closure (N-B-26). It contains every point of the closure body and its own end marker, and the only relations assumed for it are `'static : 'b` and the implied bounds of the closure's parameter types (§7.6.5). So a closure checked against `FnMut(Str)` cannot store its argument in a captured `Vec<Str>`, and a closure checked against `Fn(Str) -> Str` cannot return a borrow of a captured variable. Either is `BORROW_ESCAPES_SIGNATURE`, with the closure as the signature. Without an expected signature, both are accepted, because the regions are then shared with the enclosing body.

Precisely, the closure has these universal regions: the regions bound by its expected signature and, for an `Fn` or `FnMut` closure, the region of its environment borrow (`borrow Self` or `borrow mut Self`, §7.11.3). Each of them, `'b`, contains every point of the closure body and its own end marker, and the conformance check of §7.9.7 applies to it with the closure as the signature, with these two additions:

1. `'b` MUST NOT contain a point outside the closure body (the body of a closure nested in it counts as inside).
2. A region that does not belong to the closure body (a region of the enclosing body, including the regions of the closure's captures) MUST NOT contain `end('b)`, unless it outlives `'b` by an implied bound of the closure's parameter types.

A violation is `BORROW_ESCAPES_SIGNATURE`. Rule 1 rejects storing an argument into captured data and returning a borrow of the environment of an `FnMut` closure. Rule 2 rejects returning captured data through a bound output region. Rust accepts some programs that rule 2 rejects: those where the outside region can be `'static`.

A closure without an expected signature has regions of the enclosing body in its signature. It can be called directly. It can also be used later where an `Fn`-family bound is required, for example as the argument of `fn run<F: Fn(borrow I32)>(f: F)`, but only if the bound's signature has no bound region at a position where the closure's signature has a region. Otherwise the callee could choose a region that the closure body does not allow, and the use is `BORROW_ESCAPES_SIGNATURE` on the expression that passes the closure. The fix is to write the closure at that place, so that it takes the bound's signature. At an output position that the bound fixes to `'static` (O4), the closure's region MUST outlive `'static`.

The closure body is checked together with the enclosing body, as one problem. Its points are part of the enclosing function's region solution, and its captures are places of the enclosing body.

### 7.11.2 Capture modes

A **captured variable** is a local of an enclosing body that the closure body mentions. The capture unit is the **whole variable**, never a field path and never a dereference (06 §6.17.3, N-C-17). A closure that writes through a `borrow mut` variable `x` (for example `x.push(1)`) therefore captures `x` itself by mutable borrow, and `x` MUST be declared `var`, although the same call outside a closure needs no `var` (§7.2.4 rule 4). A name has one mutability everywhere (N-B-18).

The capture mode of each captured place is the first one of the following that applies (D-TYPE-09):

1. **By move**, if the closure is written `move fn`, or if the body moves the place or a place rooted at it (§7.3.2).
2. **By mutable borrow**, if the body writes the place or a place rooted at it, borrows such a place mutably, or uses it as a `borrow mut` receiver. The captured place MUST be mutable (§7.2.4), otherwise `BORROW_MUT_OF_IMMUTABLE`.
3. **By shared borrow**, otherwise. This includes a variable that the body only copies.

A by-move capture of a Copy value copies it.

Capturing happens at the point where the closure expression is evaluated. The captures are created in the order of their first occurrence in the closure body (this order is used by §7.13.3):

- A by-move capture is a move access (or a read, for Copy) of the captured place.
- A by-borrow capture is a borrow of the captured place. It creates a loan, which is checked like any other (§7.9.6) and reported as `CONFLICTING_BORROW`, `ASSIGN_WHILE_BORROWED`, and so on.

### 7.11.3 Which `Fn` trait a closure implements

The capture modes determine how a closure may be called (D-TYPE-09). The trait-selection rules belong to 05. The ownership facts they depend on are these:

- If the body moves out of a by-move captured place, the closure implements only `FnOnce`.
- Otherwise, if the body writes a captured place or borrows one mutably, the closure implements `FnMut` and `FnOnce`.
- Otherwise it implements `Fn`, `FnMut`, and `FnOnce`.

A call `f(args)` of a closure value `f` uses `f` according to the most general trait it implements (N-B-19):

- For `Fn`, the call is a shared borrow of `f`.
- For `FnMut`, the call is a mutable borrow of `f`. `f` MUST be mutable: `var f = fn() { .. }`. Otherwise the call is `BORROW_MUT_OF_IMMUTABLE`, with the fix "change `let` to `var`".
- For `FnOnce` only, the call moves `f`.

If an expected bound requires `Fn` or `FnMut`, but the body moves out of a captured variable, the error is `MOVE_OUT_OF_CAPTURE`. The fix is to clone inside the body, when the type implements `Clone`.

### 7.11.4 What a closure type borrows

A closure's type carries these regions:

- for each by-borrow capture of place `c`, the loan's region together with `regions(type(c))`;
- for each by-move capture, `regions(type(c))`.

The closure value is a value like any other. While it is live (it may still be called, moved, or stored), its loans stay in scope. A closure is dropped like a record whose fields are its captures (§7.13.3).

There are no opaque return types (D-TYPE-09), so a capturing closure cannot be returned from a function that names its type. It can be passed to generic functions (`fn run<F: FnOnce()>(f: F)`). Its regions then flow through the type parameter like any other type argument.

### 7.11.5 Examples

```sai
fn closure_shared() {
    var v = Vec.new()
    v.push(1)
    let count = fn() -> USize { v.len() }    // captures v by shared borrow
    let n = count()
    v.push(2)                                // accepted: `count` is no longer live
}

fn closure_conflict() {
    var v = Vec.new()
    let count = fn() -> USize { v.len() }
    v.push(2)                                // rejected: CONFLICTING_BORROW (`count` holds a shared loan of v)
    let n = count()
}

fn closure_mut() {
    var total = 0
    var add = fn(x: I32) { total += x }      // captures total by mutable borrow; FnMut
    add(1)
    add(2)
    let t = total                            // accepted: `add` is no longer live
}

fn closure_let_fnmut() {
    var total = 0
    let add = fn(x: I32) { total += x }
    add(1)                                   // rejected: BORROW_MUT_OF_IMMUTABLE (fix: `var add`)
}

fn closure_move(name: String) {
    let greet = move fn() -> USize { name.len() }   // `name` is moved into the closure
    print_len(borrow name)                   // rejected: USE_AFTER_MOVE
}

fn closure_through_borrow(var v: borrow mut Vec<I32>) {
    var push_one = fn() { v.push(1) }        // captures v by mutable borrow, so v must be `var`
    push_one()
}

fn closure_through_borrow_let(v: borrow mut Vec<I32>) {
    var push_one = fn() { v.push(1) }        // rejected: BORROW_MUT_OF_IMMUTABLE (fix: `var v`)
    push_one()
}
```

---

## 7.12 Loops, returned borrows, and mutation

### 7.12.1 Borrows in loops

A loop is a cycle in the CFG. A loan created in one iteration is in scope in the next iteration exactly when its region contains the loop's back edge, which happens when a value that holds it is live there. The rules of §7.9 apply unchanged. These consequences are worth stating:

1. A borrow whose last use is inside the iteration ends inside the iteration. A new borrow in the next iteration does not conflict with it.
2. `for x in borrow xs` holds a shared loan of `xs` for the whole loop, because the iterator is live until the loop ends (D-EXPR-11). Mutating `xs` in the body is `CONFLICTING_BORROW` or `ASSIGN_WHILE_BORROWED`.
3. Storing a borrow created in the body into a variable declared before the loop keeps the loan live as long as that variable is.
4. A move in the body of a value declared outside the loop is `USE_AFTER_MOVE` in the next iteration, unless the value is reinitialized before the back edge (§7.4.4).

```sai
fn loop_mutation(var xs: Vec<I32>) {
    for x in borrow xs {
        xs.push(*x)              // rejected: CONFLICTING_BORROW (the loop's iterator borrows xs)
    }
}

fn loop_ok(var xs: Vec<I32>) {
    let n = xs.len()
    var i: USize = 0
    while i < n {
        let x = xs[i]            // I32 is Copy; the loan through `index` ends after the copy
        xs.push(x)
        i += 1
    }
}
```

**Conditional return of a borrow.** Outlives constraints are location-insensitive (§7.9.4). So a borrow that flows into the return value on one path is treated as live on every path from its creation, including paths that do not return it. The following is therefore rejected, even though it is safe:

```sai
fn get_or_default(v: borrow mut Vec<String>) -> borrow mut String {
    let first = borrow mut (*v)[0]
    if first.len() > 0 {
        return first
    }
    v.push(String.new())         // rejected: CONFLICTING_BORROW (`first` may be returned, so its loan covers this point)
    borrow mut (*v)[0]
}
```

Restructure so that the test does not hold the borrow: `if (*v)[0].len() > 0 { return borrow mut (*v)[0] }`. This limitation is deliberate. A location-sensitive analysis would accept more programs, but it would make acceptance depend on a much more complex procedure (P8, P9). A future revision may lift it without rejecting any program that is accepted now.

### 7.12.2 Returning borrows

A function returns a borrow only through its output region (§7.7.2). In summary:

- It may return borrows of its parameters' data that the elision rule (or its `borrows` clause) names. Callers then keep those arguments borrowed while the result is live.
- It cannot return a borrow of a local, of a temporary, or of a by-value parameter. Their storage ends when the function returns, so the error is `RETURNS_LOCAL_BORROW`. The fixes are to return the owned value instead, or to take the parameter by `borrow`.
- It cannot return a borrow of a parameter that the `borrows` clause does not list (`BORROWS_CONTRACT_VIOLATED`).
- A string literal, or a borrow from another `'static` source, may always be returned.

### 7.12.3 Mutation through `var` and through `borrow mut`

There are two ways to mutate. They are not interchangeable:

| | `var x` | `borrow mut` |
|---|---|---|
| What can be written | `x` and every place owned along its path from `x` | the pointee `*r` and the places owned along its path from `*r` |
| Who sees the change | only the owner. A `var` parameter is the function's own value, so the caller does not see changes | the owner of the borrowed place, after the borrow ends |
| Can the binding be re-pointed? | yes: `x = new_value` | only if the binding holding `r` is itself `var`: `var r = borrow mut a` then `r = borrow mut b` |
| Requirements | none | the borrowed place is mutable (§7.2.4); no conflicting loan |

Assigning through a mutable borrow (`*r = v`) first drops the old value of `*r`. The old value is always initialized, because places behind a borrow are never uninitialized (§7.4.1). To take the old value out instead of dropping it, write `core.mem.replace(r, v)`.

---

## 7.13 Drop

### 7.13.1 When values are dropped

An owned value is dropped in exactly these situations, and only if it is still owned (initialized) at that moment:

1. Its owning local goes out of scope (§7.13.2).
2. It is the old value of a place that is assigned to (§7.3.6 step 4).
3. It is a temporary whose temporary scope ends (06; D-OWN-05).
4. It is a by-value part of a scrutinee or initializer temporary that a pattern did not bind (§7.3.2).
5. It is passed to `core.mem.drop(x)`. That function takes `x` by value and lets it go out of scope (16).
6. It is a component of a value that is being dropped (§7.13.3).

A value is **not** dropped in these situations:

- It has been moved out (§7.13.5).
- It was passed to `core.mem.forget` (16). Leaking is safe, but a forgotten resource handle is never closed.
- Execution traps (D-ERR-01). There is no unwinding. When a trap happens, no further code of the instance runs, including `Drop` impls.
- It lives in an arena (§7.15). Arena values are Copy and have no drop glue.

### 7.13.2 Scope-end drop order

When control leaves a scope, normally or through `break`, `continue`, `return`, or `?`, the owned locals of that scope are dropped in **reverse declaration order** (D-OWN-05). If several scopes are left, the innermost is handled first. These rules make the order exact:

1. The declaration order of locals is the order of their `let` / `var` statements.
2. The bindings introduced by one pattern are declared in the source order of the binding names. So they are dropped in reverse source order (N-B-20).
3. Parameters are locals of the function's outermost scope, declared in parameter order before every body local. So they are dropped after all body locals, in reverse parameter order, and `self` is dropped last.
4. The return value is moved to the caller and is not dropped.
5. A local whose type has no drop glue is not dropped. Only its storage ends.
6. A loop body is a scope, so its locals are dropped at the end of every iteration.
7. Match-arm bindings are dropped at the end of their arm.
8. The order of temporaries, including those of a block's tail expression and of a `match` scrutinee, belongs to 06. Statement temporaries are dropped at the end of the statement, in reverse order of creation (D-OWN-05).

After a local is dropped, its storage ends (`StorageDead`, §7.9.2).

### 7.13.3 Drop glue: order inside a value

Dropping a value `v` of type `T` runs its **drop glue**:

| `T` | Drop glue |
|---|---|
| a nominal type with a `Drop` impl | first `Drop.drop(borrow mut v)`, then the drop glue of the fields in declaration order |
| a record type without a `Drop` impl | the fields in declaration order (D-OWN-05) |
| an enum | the fields of the active variant, in declaration order |
| a resource | its `Drop` impl, or the resource-drop of an imported handle (D-OWN-06), then its fields in declaration order |
| a tuple, an `Array` | the elements in ascending index order (N-B-21) |
| `Box<T>` | the contents, then the deallocation |
| `Vec<T>`, `String` | the elements in ascending index order, then the deallocation (16) |
| a closure | its by-move captures, in capture order (§7.11.2) |
| a future (Stage 5) | as defined in 11 |
| a Copy type, a view type, a function pointer, a raw pointer | nothing; there is no drop glue |

A type **has drop glue** if it has a `Drop` impl, is a resource, is `Box`, `Vec`, or `String`, or has a component with drop glue.

If a value without a `Drop` impl has been partially moved, only its still-initialized fields are dropped, in declaration order. Each field follows its own initialization state (§7.4.5).

### 7.13.4 Conditional drops

When the initialization state of a place at a drop point depends on the path taken, the drop is guarded by a drop flag (§7.4.5). The flag is set when the place is initialized and cleared when it is moved out. The drop runs if the flag is set.

```sai
fn conditional(s: String, c: Bool) {
    if c {
        consume(s)
    }
} // `s` is dropped here only if `c` was false
```

### 7.13.5 Moving out disables drop

Moving a value transfers the duty to drop it to the new owner. The old owner never drops it. This is how values pass between functions, into containers, and into closures without being dropped twice.

Because a `Drop` impl runs on the whole value and sees all its fields, a field cannot be moved out of a value whose type implements `Drop` (`MOVE_OUT_OF_DROP_TYPE`, §7.3.5). Use `core.mem.take` or `core.mem.replace` on the field. Inside the `drop` method itself, `self` is a `borrow mut Self`, so the same tools apply.

### 7.13.6 Restrictions on `Drop`

The `Drop` trait is a lang item of 16, with the method `fn drop(self: borrow mut Self)`. An impl of it is subject to these rules:

1. **D1.** `Drop.drop` MUST have no effects (`effects {}`, D-OWN-05). Deallocation is not an effect. An effect in the body is `DROP_HAS_EFFECTS`, which the effect analysis reports ([08](08-effects-and-capabilities.md) §8.6.3). Fallible or effectful cleanup is an explicit method, by convention `fn close(self: Self) -> Result<(), E>` (D-OWN-05).
2. **D2.** A type cannot be both Copy and `Drop`. Deriving Copy for a type with a `Drop` impl, or implementing `Drop` for a Copy type, is `COPY_AND_DROP`.
3. **D3.** `Drop` may be implemented only for a record type, an enum, a distinct type, or a resource defined in the same package. Otherwise the error is `DROP_IMPL_INVALID_TYPE`. The impl MUST be generic over exactly the type's own generic parameters, with no bounds beyond those of the type's declaration. Otherwise the error is `DROP_IMPL_NOT_GENERIC_ENOUGH`. Both rules and both codes belong to [05](05-types.md) §5.9.3 (N-A-34). A `Drop` impl applies to every instantiation of the type.
4. **D4.** `Drop.drop` cannot be called explicitly, neither as `v.drop()` nor as `Drop.drop(borrow mut v)`. Either call is `EXPLICIT_DROP_CALL`, with the fix "replace with `core.mem.drop(v)`".
5. **D5.** A `Drop` impl keeps every region of its type live up to the drop (§7.13.7). A value with a `Drop` impl that holds a borrow MUST therefore be dropped before the borrowed data is.

### 7.13.7 Drop-liveness

`droplive(T)` is the set of regions that MUST still be valid when a value of type `T` is dropped:

- If `T` has no drop glue: `∅`.
- If `T` is a nominal type with a `Drop` impl, a resource, or a future: `regions(T)`. The drop code may read every borrow inside the value.
- If `T` is `Box<U>` or `Vec<U>`: `droplive(U)`. These types are **drop-transparent**: their drop glue only drops the contents and frees memory, so it never reads a borrow inside `U`. 16 MAY designate further core or alloc types as drop-transparent. User code cannot (this is a deliberate freedom of the library, N-B-22).
- Otherwise (records and enums without `Drop`, tuples, arrays, closures): the union of `droplive` of the components.

```sai
fn drop_transparent() {
    var names: Vec<Str> = Vec.new()
    let text = "a b".to_string()
    names.push(text.as_str())
} // accepted: `text` is dropped first; `names` is dropped later,
                                 // but dropping a Vec<Str> reads no Str (droplive is empty)
```

---

## 7.14 Resources (Stage 4)

A `resource` (D-OWN-06; 03 `ResourceDecl`) is a nominal type with identity, and it is **affine**:

1. It is never Copy. Writing `@derive(Copy)`, or any other `@derive`, on a resource is `DERIVE_NOT_SUPPORTED` ([05](05-types.md) §5.6.6).
2. Each use by value moves it, so it is used by value at most once. It may also never be used by value, in which case it is dropped implicitly (§7.13). There are no linear types in v0.1. Nothing forces a resource to be consumed explicitly (D-OWN-06; O-03).
3. Dropping it runs its `Drop` impl if it has one. For an imported handle, dropping it runs the resource-drop of the handle (13).
4. For partial moves, a resource is treated as a type that implements `Drop`, even when it has no `Drop` impl. Its fields cannot be moved out (`MOVE_OUT_OF_DROP_TYPE`). This keeps the identity of the resource intact (N-B-23).
5. Its fields cannot contain borrows (`RESOURCE_CONTAINS_BORROW`, §7.8.4).
6. Borrowing a resource (`borrow File`) is an ordinary borrow. At a component boundary it maps to a WIT `borrow<..>` handle (13).

```sai
resource File {
    fd: U32
}

fn size(file: borrow File) -> U64 {
    0
}

fn close(file: owned File) { }

fn use_file(f: File) {
    let n = size(borrow f)
    close(f)                     // moves f; close drops it
    let m = size(borrow f)       // rejected: USE_AFTER_MOVE
}
```

---

## 7.15 Allocation regions (arenas)

### 7.15.1 The `region` block

`region r { body }` (03 `RegionExpr`) is an expression. It behaves as if it were written as follows (16 owns the exact desugaring):

1. A fresh hidden local `arena` of the lang-item type `Arena` is created at block entry. It holds the arena's chunks.
2. `r` is bound to `borrow arena`, of type `borrow Arena` (D-OWN-07).
3. `body` is evaluated, and its value is the value of the whole expression.
4. At block exit, `r` and then `arena` go out of scope. Dropping `arena` frees every chunk at once.

So the shared loan of `arena` held by `r` and by everything allocated through it has to end before block exit. Otherwise the drop of `arena` conflicts with it, and the error is `BORROW_ESCAPES_ARENA` (§7.9.6). An arena block uses the `alloc` effect (D-OWN-07; 08).

### 7.15.2 The arena API

The API is a library surface (16). These are the parts the ownership rules depend on (D-OWN-07):

| Method of `Arena` | Signature | Effects |
|---|---|---|
| `alloc` | `fn alloc<T: Copy>(self: borrow Self, value: T) -> borrow mut T` | `alloc` |
| `alloc_str` | `fn alloc_str(self: borrow Self, s: Str) -> Str` | `alloc` |
| `alloc_slice` | `fn alloc_slice<T: Copy>(self: borrow Self, s: Slice<T>) -> Slice<T>` | `alloc` |

Every result borrows the arena, not the argument: by rule (b), the output is outlived by the regions of `self` only. `alloc_str` and `alloc_slice` copy the data into the arena. So their results stay valid after the source is gone, for as long as the arena is.

`alloc` returns a `borrow mut T` from a shared `borrow Arena`. That is sound because each call returns fresh, disjoint memory. The library implements it with unsafe code, which changes the arena's own fields under the shared loan of `r`. That is the arena exception of [12](12-unsafe-and-ffi.md) §12.6.4 rule 5, which safe code cannot observe ([16](16-core-library.md) §16.15.5 item 4). Every allocated value is Copy, so it has no drop glue, and freeing the arena drops nothing (D-OWN-07). Allocating a non-Copy value is the bound failure `ARENA_REQUIRES_COPY`. The type checker reports it (05). The structured fix suggests allocating a Copy representation, or using `Box` / `Vec` outside the arena.

Allocation is always explicit. Ordinary allocations inside the block (`Vec.new()`, `to_string()`) use the global allocator, not the arena (D-OWN-07).

### 7.15.3 Escape rules

All escape checking is done by the ordinary borrow rules (D-OWN-07), applied to the arena's loan. A value whose type carries the region of the arena's loan (or a region that must outlive it) cannot be live at block exit. The following are rejected:

- the block's value carrying the region of `r` (for example `region r { r.alloc(1) }`);
- assigning such a value to a variable declared outside the block that is used after the block;
- passing `r` to a function that stores it in data outside the block.

`r` can be passed to helper functions. By rule (c), a helper `fn node(r: borrow Arena, v: I32) -> borrow mut Node` returns a borrow of the caller's arena.

### 7.15.4 Examples

```sai
@derive(Copy, Clone)
type Point {
    x: I32
    y: I32
}

fn arena_ok() -> I32 {
    region r {
        let p = r.alloc(Point(x: 1, y: 2))
        p.x = 10
        let q = r.alloc(Point(x: 3, y: 4))
        p.x + q.y                // I32 does not carry the arena's region: accepted
    }
}

fn arena_escape() -> I32 {
    var keep = borrow 0
    region r {
        let p = r.alloc(5)
        keep = borrow *p         // rejected: BORROW_ESCAPES_ARENA (`keep` is used after the block)
    }
    *keep
}

fn arena_non_copy() {
    region r {
        let s = r.alloc("x".to_string())    // rejected: ARENA_REQUIRES_COPY (fix: `r.alloc_str("x")`)
    }
}
```

Nested arenas follow from the same rules. A borrow of an outer arena can be stored in values inside an inner block, but not the other way round.

---

## 7.16 Borrows across `.await` (Stage 5)

The async model (state machines, polling, executors, cancellation, `async.scope`) belongs to 11 (D-ASYNC-01). These are the ownership rules:

1. **The operand is moved.** `e.await` uses `e` by value (§7.3.2). The future is moved into a hidden slot of the enclosing frame, and it is dropped there when it completes (D-ASYNC-01).
2. **Borrows may be held across `.await`.** An `.await` is an ordinary point of the CFG (§7.9.1 item 6). A loan of a frame-local value that is in scope before an `.await` stays in scope after it, under the same rules as across a call. This is allowed because the frame does not move after it is first polled (the `poll` contract, D-ASYNC-01), so no Pin is needed.
3. **The future of an `async fn` captures every parameter** (N-B-25). The future type carries the regions of all parameter types (§7.6.2). Calling `f(borrow x)` on an `async fn f` keeps `x` borrowed until the returned future is dropped, whether or not the future has been awaited yet. The `borrows` clause and the elision rules of §7.7.2 apply to the declared output type `T` of `Future<Output = T>`.
4. **An `async` block** captures like a closure (§7.11.2). `async move { .. }` captures by move.
5. **Dropping a future** cancels it (D-ASYNC-01). It is a drop access with `droplive = regions(future type)` (§7.13.7). The drop order of the locals in a suspended frame is defined in 11. It follows §7.13.2 for the scopes that are active at the suspension point. For the borrow rules, every `.await` therefore has a second successor in the CFG, its **cancellation exit**: the awaited future is dropped, then the drops and storage ends that a `return` at that point would perform run, and the body is left (N-B-27). The rules of §7.9 apply to this path like to any other. So a value whose `Drop` impl reads a borrow of another local must be dropped before that local on the cancellation exit too, even if the normal path moves it away first.
6. **Children of `async.scope`.** A borrow that a child task holds must outlive the scope, because the scope waits for every child. The signature of `scope` cannot express this. The typing rule of [11](11-async-and-concurrency.md) §11.7.3 (N-I-11) does. The rules for exclusive and shared borrows shared between concurrent children belong to 11.

```sai
async fn fill(buf: borrow mut Vec<Byte>, src: borrow Source) -> USize
    effects { net.http }
{
    let first = borrow mut (*buf)[0] // a borrow of the caller's data
    let n = src.read().await // held across `.await`: accepted
    *first = 0
    n
}
```

---

## 7.17 Unsafe code and raw pointers

Raw pointers are outside the borrow checker (D-UNSAFE-01; 12):

1. Creating a raw pointer from a place (the operation of 12 that yields `Ptr<T>` or `PtrMut<T>`) is a deep read access (for `Ptr`) or a deep write access (for `PtrMut`) of the place, at that point. It creates **no loan**. The pointer carries no region, and nothing keeps the place borrowed afterwards.
2. A `Deref` of a raw pointer is allowed only in unsafe code (12). Places behind a raw pointer are not move paths (§7.4.1). Their initialization is not tracked, and they cannot be moved out of (`MOVE_OUT_OF_BORROW`). Use the unsafe read and write operations of 12.
3. `borrow *p` and `borrow mut *p` for a raw pointer `p` create a loan of `*p`. Its region has no reborrow constraint (§7.9.3), so it may be as long as the context requires. Choosing a valid region is the obligation of the unsafe code (12).
4. `unsafe` does not turn the checker off. Every rule of this chapter applies inside unsafe blocks and unsafe functions to every place that is not behind a raw pointer.
5. Region-changing transmutes and the aliasing obligations that unsafe code must uphold, so that safe code remains sound, belong to 12.

---

## 7.18 Diagnostics and structured fixes

Diagnostics follow D-TOOL-01 and the model of [impl/11](../impl/11-diagnostics.md): stable codes, JSON output, and structured fixes that refer to semantic node IDs.

### 7.18.1 Primary span

| Code family | Primary span |
|---|---|
| `USE_AFTER_MOVE` | the use |
| `MOVE_OUT_OF_*` | the expression that would move |
| `ASSIGN_TO_IMMUTABLE`, `PARTIAL_ASSIGN_TO_MOVED` | the assigned place |
| `BORROW_MUT_OF_IMMUTABLE` | the `borrow mut` expression, the auto-referenced receiver, the capturing closure, or the call of the closure |
| `CONFLICTING_BORROW`, `USE_WHILE_MUT_BORROWED`, `ASSIGN_WHILE_BORROWED`, `MOVE_WHILE_BORROWED` | the conflicting access |
| `DOES_NOT_LIVE_LONG_ENOUGH`, `TEMPORARY_DROPPED_WHILE_BORROWED` | the borrow that created the loan |
| `RETURNS_LOCAL_BORROW` | the returned expression (or the tail expression) whose value carries the loan's region, or a region that the loan's region must outlive, into the return place |
| `BORROW_ESCAPES_ARENA` | the first point where a value that carries the arena's region flows into a place that is live after the block, or the block's tail expression |
| `BORROWS_CONTRACT_VIOLATED`, `BORROW_ESCAPES_SIGNATURE` | the value flow (assignment, argument, or return) that requires the undeclared relation |
| signature diagnostics (`BORROWS_*`, `IMPL_SIGNATURE_REGION_MISMATCH`, `AMBIGUOUS_IMPL_REGION`, `RESOURCE_CONTAINS_BORROW`) | the clause, type, or field concerned |
| `FN_POINTER_REGION_MISMATCH` | the coerced function item expression |

### 7.18.2 Naming regions

Messages never contain `'a` syntax that the user could copy into source. A region is described by the place or value it belongs to: "the borrow of `text`", "the result of `rest`", "the arena `r`". When two regions of one signature must be told apart, they are numbered `'1`, `'2`, ... in the order of §7.7.1, and the message explains each one ("`'1` is the borrow of `self`; `'2` is the data `self` holds").

### 7.18.3 The three-point explanation

Every loan-conflict diagnostic (§7.9.6) MUST carry three labeled spans:

1. **Created:** where the conflicting loan was created, with its kind ("`v` is borrowed here (shared)").
2. **Conflict:** the primary span ("`v` is borrowed mutably here, while the shared borrow is live").
3. **Later use:** a use that keeps the loan in scope after the conflict ("the shared borrow is used later here"). This is the earliest point, in a deterministic order, that is reachable from the conflict and that makes the loan's region contain the conflict point. If the loan is kept live only by a drop, the label says so ("... is used later when `names` is dropped"). If it is kept live by an end marker, the label names the signature ("... is returned here, so it must outlive the call").

The selection procedure is in [impl/05](../impl/05-borrow-checker.md) §5.16.

### 7.18.4 Structured fixes

| Fix | Edit | Offered for |
|---|---|---|
| add `.clone()` | append `.clone()` to the moved expression | `USE_AFTER_MOVE` (at the earliest move point that reaches the use) and `MOVE_OUT_OF_BORROW`, when the type implements `Clone` |
| add `borrow` | insert `borrow ` or `borrow mut ` before an argument | `MISSING_BORROW`. For `USE_AFTER_MOVE` too, when the move is an argument to a parameter of type `T` and the callee is local to the package (the fix then also changes the parameter type) |
| change `let` to `var` | replace the `let` keyword (or add `var` to a pattern binding) | `ASSIGN_TO_IMMUTABLE`, `BORROW_MUT_OF_IMMUTABLE` |
| use `core.mem.take` / `replace` | rewrite `p` as `core.mem.take(borrow mut p)` | `MOVE_OUT_OF_BORROW`, `MOVE_OUT_OF_DROP_TYPE`, when the type implements `Default` (take) |
| use `Box.into_inner` | rewrite `*b` as `Box.into_inner(b)` | `MOVE_OUT_OF_BOX` |
| bind by `borrow` | change a binding `x` in a pattern to `borrow x` | `MOVE_OUT_OF_BORROW` in patterns (D-EXPR-04) |
| bind the temporary | insert `let tmp = <expr>` before the statement and use `tmp` | `TEMPORARY_DROPPED_WHILE_BORROWED`, and argument-order conflicts (§7.10) |
| add `move` | insert `move` before `fn` of a closure | `DOES_NOT_LIVE_LONG_ENOUGH` when the loan is a closure capture and the closure is used after the captured local's scope |
| add provenance | add a path to `borrows { .. }`, or create the clause | `BORROWS_CONTRACT_VIOLATED` |
| remove clause or entry | delete the `borrows` clause or the redundant entry | `BORROWS_WITHOUT_OUTPUT_REGION`, `REDUNDANT_PROVENANCE` |
| use `core.mem.drop` | replace `x.drop()` with `core.mem.drop(x)` | `EXPLICIT_DROP_CALL` |
| write `Self.Item` | replace the written type in an impl method's signature | `IMPL_SIGNATURE_REGION_MISMATCH` caused by an associated type |
| write the closure in place | replace the closure variable with the closure expression, and remove its `let` | `BORROW_ESCAPES_SIGNATURE` for a closure without an expected signature that is passed where an `Fn`-family bound binds a region (§7.11.1), when the variable is used only there |

A fix is **machine-applicable** only if it meets the conditions of [impl/11](../impl/11-diagnostics.md) §11.7.2 (it is the only correct repair and it cannot change run-time behavior) and, in addition, applying it makes the diagnostic disappear and introduces no new error in the program as it was checked. Otherwise it is offered as a suggestion only. The procedure that decides this is in [impl/05](../impl/05-borrow-checker.md) §5.16.

---

## 7.19 Correspondence to Rust

Every program accepted by this chapter translates to a Rust program with explicit lifetimes that the Rust borrow checker (NLL) accepts (D-OWN-03). The translation works as follows:

- Each elided universal region becomes a named lifetime parameter.
- The output assumptions become `'x: 'o` bounds.
- Each implicit type region becomes the single lifetime parameter of the corresponding Rust type.
- Views become `&'a T`, `&'a mut T`, `&'a str`, `&'a [T]`, and `&'a mut [T]`.
- Explicit borrows become `&` / `&mut`.
- `Drop` impls on types that hold borrows become impls without `#[may_dangle]`. `Box` and `Vec` keep their standard `#[may_dangle]` behavior (drop-transparency, §7.13.7).

Soundness of safe SAI is argued through this correspondence. A rule that would accept a program with no such translation is a defect of this chapter. The correspondence also explains why generic bodies are checked once, before monomorphization (§7.6.5).

---

## 7.20 Decisions introduced by this chapter

decisions.md is silent on the following points. They are decided here by the principles P1–P9 and are provisional until they are merged into decisions.md.

| ID | Rule | Rationale | Section |
|---|---|---|---|
| N-B-1 | There is no static promotion. A constant is a value expression, and `borrow C` borrows a temporary. `'static` data comes only from string literals, byte string literals (`Slice<Byte>`), outputs with no input region (N-B-11), library functions that 16 documents, and unsafe code. | P8: one rule for every value expression. P1: no implicit static data. D-CAP-02 already rules out `static` items. | §7.2.1, §7.5.7 |
| N-B-2 | A non-Copy value cannot be moved out of `*b` for `b : Box<T>`. Use `Box.into_inner(b)` (`MOVE_OUT_OF_BOX`). | P8, P9: `Box` is an ordinary library type that implements `Drop`, with no special move semantics. | §7.3.5 |
| N-B-3 | Assigning to a strict sub-place of a place that is not definitely initialized is `PARTIAL_ASSIGN_TO_MOVED`. Assigning exactly the moved-out path reinitializes it. | P2, P9: no partially initialized aggregates appear from nothing. | §7.3.6 |
| N-B-4 | A non-Copy value cannot be moved out of `a[i]` with the built-in index (`MOVE_OUT_OF_INDEX`). Slice patterns (`ConstIndex`) on owned `Array` locals may move. | P2: the checker does not track dynamic indices (determinism principle). | §7.3.5 |
| N-B-5 | An implicit reborrow happens only if the expected type at a coercion site is `borrow mut U` (or `SliceMut<U>`, or a shared view for the shared coercion) **before** generic inference. A bare type parameter or an uninferred type means a move. | P8: the rule does not depend on inference order. Matches Rust. | §7.5.5 |
| N-B-6 | Temporary lifetime extension applies when the whole initializer of a `let` or `var` statement is syntactically `borrow e` or `borrow mut e` with `e` a value expression, with any pattern and an optional annotation. Nothing nested is extended. | P1, P8: one syntactic rule. It refines D-OWN-05. | §7.5.6 |
| N-B-7 | Regions inside function pointer types and `Fn`-family bounds are bound by that type (higher-ranked) and elided locally. Such types carry no free region and do not make a containing type region-carrying. | P8, P9: there is no syntax to name them. Without this, every type with a callback field would get a region. It refines the "syntactically contains a view type" wording of D-OWN-03. | §7.6.2, §7.7.6, §7.8.2 |
| N-B-8 | The variance of a user type's region and type parameters is computed from its fields. An unused parameter is invariant. | P2, P8: variance is derived, never declared. Invariant is the safe default. | §7.6.3 |
| N-B-9 | Region subtyping applies at every value flow, not only at the coercion sites of D-TYPE-04. | P8, P9: regions have no run-time effect, and the least solution is then the most permissive one. | §7.6.3 |
| N-B-10 | All region positions of a return type share one output region `'o`. | It follows D-OWN-03's singular "the output region". P9. | §7.7.2 |
| N-B-11 | If a function has no input region, its output region is `'static`. | P8: rule (c) with an empty set of sources has only this meaning. | §7.7.2 O4 |
| N-B-12 | In a trait impl, region positions in associated type definitions and trait arguments denote the target type's single region, or `'static` if it has none. If it has more than one, the error is `AMBIGUOUS_IMPL_REGION`. | P1, P8: there is no syntax, so the rule must be unique and predictable. | §7.7.5 |
| N-B-13 | After elision and substitution, an impl method's signature must be identical to the trait method's, including regions (`IMPL_SIGNATURE_REGION_MISMATCH`). | P8, P9: no refinement of trait signatures in v0.1. | §7.7.5 |
| N-B-14 | A resource cannot carry a region (`RESOURCE_CONTAINS_BORROW`). | P4: a WIT handle can outlive any borrow of linear memory. D-OWN-06. | §7.8.4 |
| N-B-15 | A drop is a write-like access that conflicts with shared as well as mutable loans. It is deep, except that it reaches data behind a stored `borrow mut` only through a `Drop` impl or a resource on the way, and never conflicts with a loan behind a shared view. | P2: soundness. It completes the list of D-OWN-04. P9: like Rust, dropping a record that holds a `borrow mut` does not end reborrows through it. | §7.9.2, §7.9.6 |
| N-B-16 | The base of an `IndexMut` place is an auto-referenced receiver, so it is a two-phase borrow. A `borrow mut` receiver that is reborrowed implicitly is two-phase too. | It applies D-OWN-04 ("auto-ref'd `borrow mut` receivers") to every auto-ref. P9. | §7.10 |
| N-B-17 | A closure takes its regions from an expected `Fn`-family bound. Otherwise its signature regions are local region variables of the enclosing body, and the closure satisfies a later `Fn`-family bound only where that bound binds no region. | P8: no independent elision for closures. P2: a callee cannot choose a region that the closure body did not allow. | §7.11.1 |
| N-B-18 | The capture unit is the whole variable, also for a `borrow mut` variable that the body uses only through `*x`. Mutating through such a variable in a closure is a mutable capture of the variable, which requires `var`. | P8: one capture unit, as in 06 N-C-17 and D-TYPE-09 ("each variable"); a name has one mutability everywhere. The cost is a `var` on such parameters. | §7.11.2 |
| N-B-19 | A call `f(..)` of a closure value borrows `f` (for `Fn`), borrows it mutably (for `FnMut`, which requires `var f`), or moves it (for `FnOnce` only). | P1: mutation of the environment is visible at the binding. | §7.11.3 |
| N-B-20 | The bindings of one pattern are dropped in reverse source order. Parameters are dropped after all body locals, in reverse parameter order. | P8: an exact order, consistent with D-OWN-05. | §7.13.2 |
| N-B-21 | Tuple and `Array` elements are dropped in ascending index order. | P8. The same as struct fields (declaration order). | §7.13.3 |
| N-B-22 | `Box` and `Vec` are drop-transparent, and 16 may designate further core/alloc types. User types cannot be drop-transparent. | P2, P9: it accepts the common `Vec<Str>` pattern without an unsafe attribute in user code. | §7.13.7 |
| N-B-23 | For partial moves, a resource is treated as a type that implements `Drop`. | P8: a resource keeps its identity until it is dropped as a whole. | §7.14 |
| N-B-24 | Outlives constraints are location-insensitive (`'a : 'b` means `'a ⊇ 'b` over the whole body). A conditionally returned borrow is therefore rejected on every path from its creation. | P8, P9, determinism: the simplest standard formulation (NLL). A future relaxation accepts strictly more programs. | §7.9.4, §7.12.1 |
| N-B-25 | The future of an `async fn` carries the regions of all its parameters. Dropping a future keeps all its regions live (`droplive = regions`). | P2: a future may use any parameter until it completes. 11 may refine this without weakening it. | §7.16, §7.13.7 |
| N-B-26 | A region bound by a closure's expected signature, and the region of an `Fn` / `FnMut` closure's environment borrow, are universal regions of the closure body. Only `'static : 'b` and the implied bounds of the closure's parameter types are assumed for them. They contain no point outside the closure body, and no region outside the closure body contains their end markers unless an implied bound allows it. A projection `P.A` satisfies `P.A : 'l` whenever `P : 'l` holds. | P2: the caller of the closure chooses the region, so the body may assume nothing else. P8: no propagation of requirements out of closures. | §7.11.1, §7.6.5 |
| N-B-27 | Every `.await` has a cancellation exit in the CFG of the borrow rules. It drops the awaited future and then performs the drops and storage ends of a `return` at that point. | P2: soundness of cancellation (D-ASYNC-01), which drops a suspended frame in scope order. | §7.16 |
| N-B-28 | A `Subslice` of an owned `Array` (a by-value `..rest` binding of a slice pattern) is a partial move of the `ConstIndex` move paths of the positions it covers. | P8: one rule for all slice-pattern moves. It matches Rust. | §7.3.5, §7.4.1 |

---

## 7.21 Diagnostics

All codes are errors unless marked otherwise. Codes defined in another document are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `USE_AFTER_MOVE` | error | a place is used after it has been moved out, or may have been on some path (§7.4.4) |
| `MOVE_OUT_OF_BORROW` | error | a non-Copy value would be moved out from behind a borrow or a raw pointer (§7.3.5, D-OWN-01, D-EXPR-04) |
| `MOVE_OUT_OF_DROP_TYPE` | error | a field would be moved out of a value whose type implements `Drop`, or out of a resource (§7.3.5, §7.14) |
| `MOVE_OUT_OF_BOX` | error | a non-Copy value would be moved out of `*b` for a `Box` (§7.3.5, N-B-2) |
| `MOVE_OUT_OF_INDEX` | error | a non-Copy value would be moved out of a built-in index place `a[i]` (§7.3.5, N-B-4) |
| `PARTIAL_ASSIGN_TO_MOVED` | error | a field of a value that is not definitely initialized is assigned (§7.3.6, N-B-3) |
| `ASSIGN_TO_IMMUTABLE` | error | assignment to a place that is not mutable (§7.2.4) |
| `BORROW_MUT_OF_IMMUTABLE` | error | `borrow mut` (explicit, receiver, capture, or closure call) of a place that is not mutable (§7.2.4, §7.5.1, D-OWN-02) |
| `MISSING_BORROW` | error | an argument is passed without the `borrow` / `borrow mut` its parameter type requires. It is reported by the type checker (§7.5.2, D-OWN-02) |
| `CONFLICTING_BORROW` | error | a borrow (including a receiver, a two-phase activation, or a capture) conflicts with a live loan (§7.9.6) |
| `USE_WHILE_MUT_BORROWED` | error | a place is read while it is mutably borrowed (§7.9.6) |
| `ASSIGN_WHILE_BORROWED` | error | a place is assigned (or its old value dropped) while it is borrowed (§7.9.6) |
| `MOVE_WHILE_BORROWED` | error | a place is moved while it is borrowed (§7.9.6) |
| `DOES_NOT_LIVE_LONG_ENOUGH` | error | a local is dropped or its storage ends while a loan of it is live (§7.9.6) |
| `TEMPORARY_DROPPED_WHILE_BORROWED` | error | a temporary is dropped at the end of its scope while a loan of it is live (§7.5.6, §7.9.6) |
| `RETURNS_LOCAL_BORROW` | error | the returned value borrows a local, a temporary, or a by-value parameter (§7.7.4, §7.12.2) |
| `BORROWS_CONTRACT_VIOLATED` | error | the returned value borrows from a source that the elision rules or the `borrows` clause do not name (§7.7.4, §7.9.7) |
| `BORROW_ESCAPES_SIGNATURE` | error | the body needs an outlives relation between input regions, or a type-parameter bound, that the signature does not imply (§7.9.7, §7.6.5) |
| `BORROW_ESCAPES_ARENA` | error | a value that borrows an arena is live after its `region` block ends (§7.15) |
| `ARENA_REQUIRES_COPY` | error | a non-Copy value is allocated in an arena. It is reported by the type checker (§7.15.2, D-OWN-07) |
| `BORROWS_INVALID_PROVENANCE` | error | a `borrows` entry does not name a parameter, a valid accessible field path, or a source that carries a region (§7.7.3) |
| `BORROWS_WITHOUT_OUTPUT_REGION` | error | a `borrows` clause on a function whose return type has no region (§7.7.3) |
| `REDUNDANT_PROVENANCE` | warning (lint) | a `borrows` entry duplicates another, or is subsumed by a prefix (§7.7.3) |
| `AMBIGUOUS_IMPL_REGION` | error | a region position in an associated type or trait argument of an impl whose target carries several regions (§7.7.5, N-B-12) |
| `IMPL_SIGNATURE_REGION_MISMATCH` | error | an impl method's elided signature differs from the trait method's in its regions (§7.7.5, N-B-13) |
| `RESOURCE_CONTAINS_BORROW` | error | a resource field type contains a borrow (§7.8.4, N-B-14) |
| `FN_POINTER_REGION_MISMATCH` | error | a function item is coerced to a function pointer type whose elided regions differ from the item's (§7.7.6) |
| `DERIVE_NOT_SUPPORTED` | error | defined in [05](05-types.md) §5.12.4; here, Copy is derived for a resource (§7.14, D-OWN-06) |
| `MOVE_OUT_OF_CAPTURE` | error | a closure required to be `Fn` or `FnMut` moves out of a captured variable (§7.11.3) |
| `COPY_AND_DROP` | error | a type is both Copy and `Drop` (§7.13.6) |
| `DROP_HAS_EFFECTS` | error | defined in [08](08-effects-and-capabilities.md) §8.6.3 and reported by the effect analysis; a `Drop.drop` impl has effects (§7.13.6, D-OWN-05) |
| `DROP_IMPL_INVALID_TYPE` | error | defined in [05](05-types.md) §5.9.3; a `Drop` impl for a type that is not a local record, enum, distinct type, or resource (§7.13.6) |
| `DROP_IMPL_NOT_GENERIC_ENOUGH` | error | defined in [05](05-types.md) §5.9.3; a `Drop` impl that is not generic over exactly the type's own parameters and bounds (§7.13.6, N-A-34) |
| `EXPLICIT_DROP_CALL` | error | `Drop.drop` is called explicitly (§7.13.6) |
| `INVALID_ASSIGN_TARGET` | error | defined in [03](03-grammar.md) §3.16; the left-hand side of an assignment is not a place expression |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14; used here for `borrow e` passed to a by-value parameter |
