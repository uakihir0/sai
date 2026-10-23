# Service Dependency Incident Correlator

This pure SAI library analyzes a fixed service graph, threshold-based metric
samples, and a short deployment history. It produces an ordered list of
investigation leads by combining each service's strongest alert, the number
of alerted downstream callers reachable through the dependency graph, and a
small bonus for a deployment shortly before the first alert.

The score ranks where an operator might investigate first. It does not prove
causation, declare an incident resolved, or trigger a rollback.

## Fixture and intended result

The fixture models seven services: an edge gateway, checkout API, catalog API,
pricing service, inventory service, primary database, and catalog cache. Nine
directed dependency edges describe caller-to-provider relationships. Metric
samples arrive as integer minutes within one fictional incident window, and
three recent deployments overlap alerts.

The intended ranking, calculated from the fixed fixture and the rules below,
is:

| Rank | Service | Score | Evidence |
|---:|---|---:|---|
| 1 | primary database | 155 | peak alert 70, six alerted downstream callers, deployment 9001 |
| 2 | pricing service | 95 | peak alert 50, two alerted downstream callers, deployment 9002 |
| 3 | checkout API | 85 | peak alert 50, one alerted downstream caller, deployment 9003 |
| 4 | inventory service | 80 | peak alert 60, two alerted downstream callers |
| 5 | catalog cache | 70 | peak alert 50, two alerted downstream callers |
| 6 | catalog API | 50 | peak alert 40, one alerted downstream caller |
| 7 | edge gateway | 50 | peak alert 50, no downstream callers |

`analysis.analyze` returns this structured report. The ranking above is
hand-checked against the algorithm; it is not the result of a successful
compiler run.

## Algorithm

1. A sample is alerting when its p95 latency is at least 400 ms or its error
   count is at least 25 per 1,000 requests. Each metric contributes 0, 20, 30,
   or 40 points at the documented thresholds in `signals.sai`.
2. Each service signal keeps the earliest alert minute, peak sample score, and
   number of alerting samples. Samples are scanned across the whole snapshot,
   so their storage order does not determine onset.
3. For every alerted service, `topology.sai` walks the graph in reverse
   caller-to-provider direction. It counts only reachable callers that also
   have an alerting signal; the candidate itself is excluded.
4. `scoring.sai` selects the latest deployment of that same service from one
   to ten minutes before its first alert. A matching deployment contributes
   25 points. Each alerted downstream caller contributes 10 points.
5. `analysis.sai` orders candidates by descending score, then earlier alert
   minute, then ascending service ID. This makes ties deterministic.

## Modules

- `model.sai` defines the bounded snapshot, graph, sample, signal, and report
  records.
- `fixtures.sai` provides the seven-service topology, eight metric samples,
  and three deployment records.
- `lookup.sai` resolves service IDs to fixed-array indexes.
- `signals.sai` applies the thresholds and aggregates per-service alerts.
- `topology.sai` computes transitive caller impact and counts alerted callers.
- `scoring.sai` correlates recent deployments and calculates candidate scores.
- `analysis.sai` builds and deterministically orders the investigation list.
- `lib.sai` exposes the fixture analysis and example regression tests.

## Limits

This bounded, in-memory example has no file, network, monitoring, paging, or
deployment integration. It does not estimate statistical causality, compare
metric baselines, de-duplicate alerts, validate live topology, model service
health beyond the supplied samples, or recommend operational actions. It
assumes unique service IDs and valid dependency, sample, and deployment
references in its fixture. Minutes are relative to one fictional incident
window; they are not calendar timestamps.

## Review required

- The alert thresholds and point weights are teaching values, not an SLO or
  paging policy. A real service team would need to choose them from its own
  metrics and operational targets.
- A deployment in the ten minutes before an alert adds 25 points. Timing is
  only a correlation hint; it does not establish that the change caused the
  alert. A deployment in the same recorded minute is excluded because the
  fixture's minute-level timestamps cannot establish that it happened first.
  The fixture intentionally has overlapping deploys to make that limitation
  visible.
- The graph stores edges as caller → provider and computes impact by walking
  them backward. The score counts transitive alerted callers once, even when
  multiple paths reach the same service.
- A service can be ranked only when at least one supplied sample crosses a
  threshold. Missing, delayed, duplicated, or malformed telemetry is outside
  this fixture's policy.
- The tie order (score, alert minute, then service ID) is deterministic for
  review and should not be treated as an operational severity policy.

## Package and compiler status

The manifest targets `wasm32-core`; this is a pure library with no declared
runtime capabilities or external packages. The source illustrates records,
enums, fixed-size arrays, `Option`, modules, loops, `@derive`, and SAI test
declarations. The current compiler's Stage 0 `sai check` and Stage 3 `sai test`
are not implemented, and `@derive` is a Stage 2 feature. Formatting can check
the source shape, but no type-check, test, build, or execution result is
claimed.
