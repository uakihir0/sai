# Multi-Region Failover Planner

Multi-Region Failover Planner builds a deterministic recovery plan for a fixed
fictional service catalog. It places services in ranked recovery regions only
when the selected replica meets the service's recovery point objective (RPO),
the dependency-aware recovery time meets its recovery time objective (RTO), and
the target region has enough failover capacity.

At incident minute 1000, the fixture produces this plan:

| Service | Wave | Recovery region | Recovered minute | Outcome |
|---|---:|---|---:|---|
| customer-db | 1 | eu-west | 1008 | Recovered |
| order-db | 1 | us-east | 1008 | Recovered |
| identity-api | 2 | eu-west | 1013 | Recovered after customer-db |
| checkout-api | — | — | — | Held: both fresh targets lack 3 free capacity units |
| catalog-cache | — | — | — | Held: only replica is 3 minutes old; RPO is 2 |
| search-index | — | — | — | Held: catalog-cache is not recovered |
| fraud-model | — | — | — | Held: 6-minute startup exceeds its 5-minute RTO |
| email-queue | — | — | — | Held: no target replica is present |
| media-renderer | — | — | — | Held: no candidate region has 6 free units |

The plan reserves 5 of 7 units in us-east and 6 of 8 units in eu-west. It leaves
ap-south unused: catalog-cache is stale, while other ready services either
prefer a different region or exceed that region's capacity. Customer-db's
preferred eu-west target includes a 2-minute activation delay, so it is
recovered at minute 1008; the fallback us-east target would finish earlier,
but the configured preference wins while both choices meet the objectives.

## Planning flow

1. `data.sai` supplies nine services, three regions, ranked failover targets,
   region-local replication checkpoints, and four dependency edges.
2. `graph.sai` rejects duplicate service, region, target, and replica identities;
   unknown region or dependency references; primary-region targets; self edges;
   duplicate dependencies; and dependency cycles before any placement is made.
3. `planner.sai` starts a new recovery wave only after all dependencies of a
   service recovered in an earlier wave. Within a wave, it considers services
   by descending priority and then ascending service ID.
4. For each service, it tests target regions by ascending preference and then
   ascending region ID. A candidate must have an available replica no older
   than the inclusive RPO, complete by the inclusive RTO, and fit the
   remaining capacity. Recovery time is the later of the incident and the
   latest prerequisite recovery, plus service startup and target activation.
5. Capacity is reserved as each service is placed. If no further ready service
   fits, each receives a stable hold reason. Holds propagate to dependent
   services as `DependenciesUnavailable`.
6. The assignment is greedy, not a global capacity optimizer. A locally
   preferred target can hold a service even when a different assignment would
   recover that service. A source-level regression demonstrates this tradeoff
   by swapping customer-db's target preference and placing order-db in us-east.
7. `lib.sai` exposes `fixture_report()`. Source-level tests pin the example
   placements, recovery times, hold reasons, capacity totals, stale-target
   fallback, future timestamp handling, overflow handling, greedy capacity
   behavior, and cycle rejection.

## Project structure and concepts

- `model.sai` defines bounded records, plan decisions, validation issues, and
  hold reasons.
- `data.sai` contains the fixed fictional outage, service catalog, regions,
  replicas, targets, and dependencies.
- `graph.sai` checks identities and produces a fail-closed dependency-cycle
  result.
- `planner.sai` assigns deterministic recovery waves and region capacity,
  evaluates inclusive RPO/RTO limits, and records explicit holds.
- `lib.sai` exposes the fixture plan and source-level contract tests.

The example illustrates bounded graph validation, topological recovery waves,
stable priority and target tie-breaking, region capacity reservation,
dependency-aware recovery time, unsigned timestamp guards, RPO/RTO checks,
fallback targets, and explicit unmet-work reporting.

## Review required

- RPO and RTO values, startup durations, activation delays, service priorities,
  region capacities, and target rankings are teaching policies. Service owners
  and incident commanders must set and approve these values from tested
  recovery objectives and actual regional capacity.
- A target's RPO age is measured at the fixed incident minute. RTO is measured
  from that minute through prerequisite recovery, service startup, and the
  selected region's activation delay. Confirm that this accounting matches the
  intended operational meaning of each objective.
- Capacity is reserved for the whole planned outage horizon and does not become
  available again when a later wave starts. This conservative example does not
  model temporary startup peaks, autoscaling, overcommit, or capacity release.
- Placement is greedy by service priority and target preference. It can leave
  a service held even when a different assignment would recover that service,
  changing which services recover. It does not maximize the number recovered.
  Confirm that deterministic local preferences are useful for this teaching
  example before treating the planner as a capacity optimizer.
- Dependencies may be placed in different regions; the planner assumes that
  their network paths, identity, secrets, and policies remain usable. It does
  not check dependency locality, network reachability, or regional isolation.
- If a service has no feasible target, the report chooses one primary reason
  with fixed precedence: no configured target, no replica, unavailable
  replica, stale or future replica, RTO or timestamp overflow, then regional
  capacity. Confirm that this ordering is useful for incident response when
  several defects apply at once.
- A `Recovered` decision means only that the in-memory fixture met these
  simplified gates. The planner does not execute failover, fence a former
  primary, elect leaders, move traffic, validate live health, or establish
  data consistency.

## Package and compiler status

This is a pure `wasm32-core` library with bounded in-memory fixtures and no
external services or persistent state. The source-level tests document the
intended policy, but they have not been run: the current compiler does not yet
support every feature exercised by the sample's semantic checks and tests.
The table above is hand-checked from the fixed data, not compiler-generated
output.
