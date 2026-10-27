# Transaction Serializability Auditor

This dependency-free SAI library audits a fixed, ordered history of at most
four committed transactions and twenty events. Each event is a read, write, or
commit against a numbered data item. The auditor adds a directed edge whenever
two different transactions access the same item in schedule order and at least
one access is a write. A deterministic topological order means the history is
conflict-serializable; a directed cycle is returned as a closed transaction-ID
witness.

## Hand-checked examples

The acyclic fixture is:

```text
position  transaction  event
0         T20          R(item 1)
1         T10          W(item 1)
2         T30          R(item 2)
3         T10          W(item 2)
4         T20          Commit
5         T10          Commit
6         T30          Commit
```

The conflicting `R20(1)` before `W10(1)` adds `T20 -> T10`. The conflicting
`R30(2)` before `W10(2)` adds `T30 -> T10`. The deterministic lowest-ID
topological result is `T20, T30, T10`; both orders among T20 and T30 are legal,
and the ID tie-break selects T20 first.

The cyclic fixture is:

```text
position  transaction  event
0         T11          R(item 1)
1         T22          R(item 2)
2         T11          W(item 2)
3         T22          W(item 1)
4         T11          Commit
5         T22          Commit
```

`R11(1)` before `W22(1)` gives `T11 -> T22`; `R22(2)` before `W11(2)` gives
`T22 -> T11`. The stable witness is `T11 -> T22 -> T11`, so this history is
not conflict-serializable. Read-read pairs do not add edges.

A four-transaction fixture extends the same pattern through T22, T33, and T44.
It checks that the bounded witness array can hold the closed cycle
`T11 -> T22 -> T33 -> T44 -> T11`.

## Modules

- `src/model.sai` defines event, history, graph-edge, status, and report
  records with fixed capacities.
- `src/history.sai` validates transaction IDs, event slots, item IDs, and
  exactly one commit after each transaction's operations.
- `src/precedence.sai` builds one stable evidence edge per ordered transaction
  pair, retaining the first conflicting event pair in schedule order.
- `src/analyzer.sai` chooses the lowest-ID ready transaction during
  topological sorting and reconstructs a deterministic cycle witness with
  bounded breadth-first searches.
- `src/fixtures.sai` supplies acyclic, cyclic, read-read-only, and malformed
  histories.
- `src/lib.sai` exposes the primary fixture and embeds assertions for the
  graph, order, witness, and fail-closed behavior.

## Input rules and bounds

Transaction IDs and item IDs must be nonzero. A history must contain one to
twenty densely populated event slots, with `None` in all unused slots. Every
transaction mentioned must have at least one read or write and exactly one
commit; no operation may follow its commit. Commit events use item ID zero.
At most four distinct transactions are accepted. Invalid or over-capacity
histories return `InvalidHistory` with no partial graph, serial order, or
cycle witness.

The graph has at most twelve directed edges because self-edges are excluded
and at most four transactions are accepted. For each ordered pair of
transactions, only the first conflicting event pair is retained as its
evidence edge. Additional conflicting items can imply the same graph edge
but cannot change serializability.

## Review required

- This is a conflict-serializability check over one already ordered,
  committed fixture. The model does not execute transactions, inspect a
  database, or establish view serializability, recoverability, strictness,
  durability, isolation-level behavior, or application invariants.
- Commit is validated as a per-transaction boundary and is not treated as a
  data-item access. Aborted, read-only-uncommitted, prepared, and in-doubt
  transaction states are outside this schedule; each listed transaction must
  commit exactly once.
- The item namespace is a nonzero integer chosen by the fixture author. There
  is no schema, key normalization, range/predicate locking, phantom detection,
  or distinction between reads of versions and reads of current values.
- The graph keeps one evidence pair for each directed transaction pair: the
  earliest pair found while scanning the history in schedule order. The
  cycle witness is stable for a given input but is not promised to be the
  shortest cycle or the only cycle. Consumers should use the listed graph
  edges to inspect the selected witness.
- Topological ties choose the lowest numeric transaction ID. This is only a
  reproducible presentation policy; it does not imply priority or execution
  order for a real database.
- Fixed capacities reject larger input instead of analyzing a prefix. The
  current algorithm's bounded pair scan and cycle search are intended for
  teaching and small fixtures, not high-volume transaction logs.

## Formatting and compiler status

Run:

```sh
sai fmt --check sample/transaction-serializability-auditor
```

The listed graph and witness are hand-calculated examples. Embedded assertions
document intended behavior, but the reference compiler currently blocks
semantic checking at its Stage 0 attribute gate and execution at its Stage 3
test-runner gate. The sample does not claim compiler-generated output or
runtime execution.
