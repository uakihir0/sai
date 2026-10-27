# Edge Cluster Capacity Planner

This sample builds a deterministic replica placement plan for a small edge
compute cluster. It reads a fixed Monday-morning demand snapshot, keeps node
safety reserves unavailable, places higher-priority services first, and
reports every replica that cannot be placed.

The fixture includes general-purpose and accelerator nodes, different CPU and
memory limits, three failure zones, and one requested pool that is absent.
Payments, telemetry, an accelerator-backed embedding worker, a report builder,
a search indexer, and an archive cache exercise priority, best-fit placement,
zone spread, and distinct hold reasons.

## Intended behavior

Run the package's command entry point to print one row per replica followed by
node utilization:

```text
Edge cluster capacity plan — Monday morning fixture
Policy: priority then service ID; best fit by remaining CPU, memory, and node ID; replicas spread across zones when requested.

  critical #100 payments-api replica 1: placed on general-z3-b (node 103)
  critical #100 payments-api replica 2: placed on general-z1-a (node 101)
  critical #100 payments-api replica 3: placed on general-z2-a (node 102)
  critical #450 embedding-worker replica 1: placed on accelerator-z1-a (node 201)
  critical #450 embedding-worker replica 2: placed on accelerator-z2-a (node 202)
  high #200 telemetry-ingest replica 1: placed on general-z1-a (node 101)
  high #200 telemetry-ingest replica 2: placed on general-z2-a (node 102)
  standard #300 report-builder replica 1: held — CPU or memory headroom is exhausted
  standard #300 report-builder replica 2: held — CPU or memory headroom is exhausted
  standard #500 search-indexer replica 1: placed on general-z3-b (node 103)
  standard #500 search-indexer replica 2: held — no unused zone has enough remaining capacity
  standard #600 archive-cache replica 1: held — no node in the required pool

Result: 8 replicas placed, 4 held for review.
```

The report also shows consumed and available CPU-millis and memory per node.
The node's configured CPU and memory reserve is excluded from available
capacity before any service is considered.

## Planning algorithm

1. Validate unique node and service IDs, reserve bounds, nonzero requests, and
   each service's one-to-four replica limit. The fixed arrays bound the plan
   to five nodes, six services, and 24 decision slots.
2. Sort services by priority (`Critical`, `High`, `Standard`), then by service
   ID. Original fixture order does not affect this order.
3. Place each service's replicas in index order. A candidate must use the
   requested node pool, fit both resource dimensions after reserves, and use
   an unused zone when the service requests zone spread.
4. Choose the best fit by least remaining CPU after placement, then least
   remaining memory, then lowest node ID. Commit that replica's CPU and memory
   usage before considering the next one.
5. If no node is selected, report `NoCompatiblePool` when the cluster has no
   node in the requested pool, `ReplicaSpreadLimit` when capacity exists only
   in already-used zones, and `CapacityExhausted` otherwise.

This is a bounded greedy planner, not a global optimizer. It does not backtrack
or move earlier replicas to improve later placements. The fixture produces 8
placements and 4 holds: two capacity holds for report-builder, one spread hold
for search-indexer, and one missing-pool hold for archive-cache.

## Project structure

- `src/model.sai` defines nodes, service demands, replica decisions, and plan
  errors.
- `src/data.sai` contains the fixed cluster and Monday-morning service demand.
- `src/planner.sai` validates input and runs the priority-ordered,
  spread-aware best-fit placement.
- `src/lib.sai` is the package root, exposes the fixture plan and command entry
  point, and declares regression assertions.
- `src/report.sai` formats decisions and remaining headroom.

## Review required

- The priority order and greedy CPU-then-memory best-fit policy are
  illustrative operator choices. A different policy can produce different
  placements; this planner does not claim globally optimal utilization.
- Node pools are hard eligibility boundaries. The fixed inventory has no
  `MemoryOptimized` node so the archive-cache replica is held. It is not
  silently reassigned to a less suitable pool.
- Zone spread requires each replica of the same service to occupy a distinct
  zone when a feasible placement remains. A later replica is held when
  capacity exists only in a zone already used by that service.
- CPU-millis and MiB are independent dimensions. The reserve is unavailable
  to planned replicas; there is no oversubscription, request bursting, or
  borrowing of reserved headroom.
- Fixtures use fixed IDs, typed values, and bounded arrays. There are no live
  metrics, service discovery, resource reservation, rollout controls,
  autoscaling, persistence, failover execution, or safety guarantee.
- The intended output and fixture totals are hand-calculated. Embedded SAI
  assertions are source-level examples; they have not been executed unless a
  compiler stage supports this sample.

## Package and compiler status

This is a `wasm32-wasi` command sample that imports `sai:process` for stdout.
The manifest follows the other process samples, with `main` declared in the
package root module `src/lib.sai`. With the existing compiler binary,
`sai fmt --check` passes. `sai check --target wasm32-wasi --tests
--offline` parses the package sources without syntax diagnostics, then reports
the Stage 0 check gate, unsupported Stage 3 `sai:process` dependency, and
unsupported `@derive` attributes. Semantic checking and the embedded source
tests are not reached; formatter/parser success does not establish runtime
correctness.
