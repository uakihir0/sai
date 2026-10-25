# Quorum Replication Lab

This bounded in-memory model follows one versioned value across three
replicas. It separates whether a replica applies a write from whether its
acknowledgement reaches the caller, then reads the newest visible version and
uses a separately scripted repair channel to update stale copies.

## Demonstration

The write uses read and write quorums of two. Replica 10 applies version 5 and
returns an acknowledgement, replica 20 is partitioned from writes, and replica
30 applies version 5 and returns the second acknowledgement. Replica 20 can
still answer reads, so it receives read repair:

```text
Write v5=55: 2 replicas applied, 2 acknowledgements -> QuorumAcknowledged
Read from replicas 10 and 20: v5=55 selected from replica 10
Read repair: replica 20 v4=40 -> v5=55
```

The ambiguous-write fixture applies the value at replicas 10 and 20, but the
reply from 20 is lost. The caller observes one acknowledgement for a required
two, even though two replicas changed:

```text
Write v5=55: 2 replicas applied, 1 acknowledgement -> Ambiguous
```

This distinguishes remote application from caller knowledge. The report
records the fixture's actual replica state; `Ambiguous` means the configured
acknowledgement threshold was not observed by the caller.

## Modules

- `src/model.sai` defines the three-replica cluster, versioned records,
  read/write/repair availability inputs, quorum thresholds, and reports.
- `src/cluster.sai` validates the fixed cluster, applies writes to reachable
  replicas, selects the newest readable record, and repairs eligible lower
  versions.
- `src/fixtures.sai` defines a successful partial write, a lost acknowledgement,
  a same-version conflict, and invalid or unavailable cases.
- `src/lib.sai` exposes the demonstrations and embeds assertions for quorum
  validation, ambiguity, deterministic selection, read repair, and conflict
  handling.

## Model boundaries and review required

- The read/write thresholds must be positive, fit within the three replicas,
  and satisfy `read_quorum + write_quorum > 3`. This overlap condition is a
  model invariant; it does not prove linearizability for an implementation.
- Versions are caller-supplied monotonic integers. A write is rejected unless
  its version exceeds every version already present in the modeled cluster,
  including on currently unavailable replicas. The sample has no version
  allocator, vector clocks, concurrent writers, or conflict-free merge rule.
- `write_reachable` means the replica applies this operation. The separate
  `acknowledgement_delivered` flag means its reply reaches the caller. A
  delivered acknowledgement without an applied write is invalid input.
- `repair_reachable` controls whether the replica accepts a read-repair
  update. It is independent of normal write reachability and read availability,
  so the scripted failure of one channel does not imply the state of another.
- Reads consider every available replica in the fixture, not exactly a chosen
  quorum subset. When versions tie, the record from the lowest replica ID is
  the deterministic candidate. If equal latest versions contain different
  values, the result is marked `ConflictingVersions` and no replica is repaired;
  the candidate is for inspection, not an automatic resolution.
- On a conflict-free read, repair copies the selected value to replicas whose
  repair channel is open and that have no record or a lower version. Replicas
  with a closed repair channel and equal-version disagreements are left
  unchanged. Repair messages, retries, and repair acknowledgements are not
  modeled.
- This is a scripted teaching state machine, not a database or consensus
  protocol. It has no network, persistence, leader election, membership changes,
  timeouts, real partial ordering, or crash durability.

## Formatting and compiler status

Run:

```sh
sai fmt --check sample/quorum-replication-lab
```

The project is a dependency-free `wasm32-core` library. Its embedded assertions
are intended model checks. The current compiler's Stage 0 `sai check` gate and
Stage 3 test-runner gate prevent semantic checking and execution of those
assertions; the trace above is derived from the fixture, not compiler output.
