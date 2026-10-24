# Canary Release Safety Planner

Canary Release Safety Planner creates a deterministic release order for four
fictional services, then evaluates each service in two regions and two rollout
cohorts. A service's prerequisites must finish every cohort in every region
before its own rollout starts. For a ready service, all regional canaries run
before any general-traffic wave.

The fixture orders services by dependencies. Identity precedes Catalog and
Payments; Catalog and Payments both precede Checkout. When multiple services
are dependency-ready, the lower numeric service ID wins, so Catalog is planned
before Payments. Region order follows the fixture: North America, then Europe.
The `Canary` cohort precedes `General`.

## Fixture result

| Seq. | Service | Region | Cohort | Decision | Finding |
|---:|---|---|---|---|---|
| 1 | Identity | North America | Canary | Proceed | All health gates pass |
| 2 | Identity | Europe | Canary | Proceed | All health gates pass |
| 3 | Identity | North America | General | Proceed | All health gates pass |
| 4 | Identity | Europe | General | Proceed | All health gates pass |
| 5 | Catalog | North America | Canary | Rollback and halt | Error rate is 180 bps; maximum is 100 bps |
| 6–16 | Catalog, Payments, Checkout | Both regions | Remaining cohorts | Halted | Earlier wave failed |

The report's expected summary is service order `[10, 20, 30, 40]`, four
proceeded waves, zero evidence holds, one rollback, eleven halted waves, and
`halt_sequence = Some(5)`. Catalog's failed canary receives a recorded
`RollbackAndHalt` decision; the report does not execute a rollback or identify
the deployed version. The remaining plan does not start. The Catalog canary's
latency and sample count pass; its error rate is the first failed gate.

The planner treats absent, duplicate, or undersampled metrics as
`HoldAndHalt`: there is not enough reliable evidence to promote the wave.
Error-rate or latency violations produce `RollbackAndHalt`. Other planned
waves then receive `Halted` with `EarlierWaveFailed`. Within one observation,
gate precedence is sample count, error rate, then p95 latency.

## Module map

- `model.sai` defines service dependencies, regions, cohorts, health evidence,
  ordered decisions, and plan-input, graph, and gate findings.
- `data.sai` supplies four services, four dependency edges, two regions, and
  sixteen fictional observations.
- `lookup.sai` finds services and counts or locates observations.
- `topology.sai` validates graph identifiers and edges, then uses a stable
  topological sort with the lowest ready service ID as its tie-breaker.
- `gates.sai` checks evidence sufficiency, error rate, and p95 latency in a
  fixed, documented order.
- `planner.sai` expands each service into regional canary and general waves,
  rejects duplicate region IDs before planning, halts at the first finding,
  and records rollback/hold and not-started waves. It also contains
  source-level tests for fixture order/counts, the first gate failure, and
  duplicate-region fail-closed behavior.
- `lib.sai` exposes `fixture_plan()` as the pure fixture entry point.

## Inspect and check

From the repository root, inspect the generated plan through the fixture table
above and trace the rules in `src/topology.sai`, `src/gates.sai`, and
`src/planner.sai`. Run `sai fmt --check sample/canary-release-safety-planner`
to verify canonical formatting. `sai check` and `sai test` do not currently
type-check or execute this project: the compiler stops at its unimplemented
Stage 0 check gate and Stage 3 test gate, respectively. The expected results
are hand-checked from the fixture and are not compiler output. The embedded
source-level tests are present for future execution, but cannot run until the
Stage 3 test implementation is available.

This is a pure `wasm32-core` library: it performs no network requests, deploys
nothing, and stores no release state. The source uses fixed-size arrays for
four services, four dependencies, two regions, and sixteen waves/observations.

## Review required

- The 100-basis-point error ceiling, 800 ms p95 latency ceiling, and cohort
  sample minimums are example policy values. A release owner must select
  service-specific thresholds and confirm the sample windows are statistically
  useful.
- The planner considers metric inputs complete snapshots. It does not model
  observation timestamps, rolling windows, confidence intervals, traffic
  weighting, metric-source availability, or stale data.
- One unhealthy observation produces an immediate rollback decision for that
  wave and a global halt. Teams should confirm whether rollback should be
  scoped to one region, one cohort, or the whole deployment and whether
  automated rollback is permitted.
- Dependencies are required to finish all regions and cohorts before a
  dependent service starts. Some systems may safely release dependent services
  alongside one another or use region-local dependency readiness.
- Service ID is the stable tie-break for independent dependencies; region and
  cohort orders come from their fixture arrays. Production plans should use an
  explicitly reviewed ordering policy.
- Region IDs must be unique. Duplicate IDs could otherwise match the same
  observation in more than one regional wave, so the planner rejects the
  entire plan with a distinct input finding and emits no service order or
  rollout waves. This fail-closed rule is a deliberate integrity choice.
- The report records an attempted rollback decision but cannot verify that a
  deployment controller executed it or that the previous version is healthy.
