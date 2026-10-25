# Bounded WAL Key-Value Store

This dependency-free SAI library models a small key-value store with a
fixed-capacity, append-only write-ahead journal. Keys are nonzero `U32` values
and values are `I32` values. The journal, transaction batches, and table all
use fixed-size arrays.

The example is deliberately an in-memory model. It does not open files, write
to a disk, or promise real crash durability. `records_to_write` lets a fixture
simulate a process stopping after a prefix of a transaction has been appended.
Recovery replays only transactions followed by a commit record, so an
uncommitted suffix cannot expose half of a multi-operation update.

## Demonstration

The fixture runs three committed transactions and then stops while writing a
fourth:

```text
tx 1: put 30=300, put 10=100, put 20=200, COMMIT
tx 2: put 10=111, delete 20, COMMIT
tx 3: put 40=400, COMMIT
tx 4: put 50=500, delete 30, <stopped before COMMIT>
```

Recovery applies transactions 1–3 and ignores both records in transaction 4.
The visible keys are `10=111`, `30=300`, and `40=400`; key 20 remains as a
tombstone in the recovered table, while key 50 was never committed.

Compaction first recovers the journal, drops tombstones and any incomplete
suffix, then writes the remaining live keys in ascending-key order as one
snapshot transaction. If no live keys remain, it returns an empty journal
instead of writing an empty transaction. Repeating compaction on the same input
produces the same journal. Compaction starts a new local transaction-ID
sequence; IDs in this teaching model are not external durable identifiers.

## Modules

- `src/model.sai` defines the store, operation batch, journal, and reports.
- `src/engine.sai` validates inputs, appends transactions, replays committed
  records, checks store invariants, and produces a canonical compacted log.
- `src/fixtures.sai` builds the committed history and interrupted tail.
- `src/lib.sai` exports the demonstration and contains recovery, tombstone,
  zero-live compaction, append-guard, and invariant assertions.

## Review required

- The final commit record is treated as an atomic marker. Real storage needs a
  specified write protocol, checksums, ordering guarantees, and flush/barrier
  semantics before a commit marker can mean durable.
- The journal permits one transaction at a time. An incomplete transaction is
  allowed only as the final suffix; appending behind it requires compaction.
- Compaction emits no commit record when the recovered state has zero live
  keys. This keeps the output canonical because empty transactions are invalid.
- Recovery rejects malformed records and committed transactions that exceed
  the fixed table capacity. It does not repair arbitrary corruption.
- Deleting a key that is absent from the recovered table is a logical no-op.
  Deleting an existing key creates a tombstone, which reserves its table slot
  until compaction.
- Keys and values are model integers. Encoding, variable-size records,
  concurrent writers, isolation between readers and writers, checksums,
  replication, and host-process failure behavior are outside this model.
- Fixed limits are six keys, six operations per transaction, and 24 journal
  records. The implementation reports capacity errors instead of partially
  appending a transaction when its complete record sequence cannot fit. It
  also checks the resulting table capacity before it appends any part of a
  transaction.

## Formatting and compiler status

Run:

```sh
sai fmt --check sample/bounded-wal-key-value-store
```

The sample uses the repository's embedded `test` declarations and `@derive`
attributes. The current reference compiler is known to stop `sai check` at the
unimplemented Stage 0 attribute gate and `sai test` at the Stage 3 test-runner
gate. Therefore the expected recovery and compaction values are documented
model behavior; they are not claimed to have been executed by the compiler.
