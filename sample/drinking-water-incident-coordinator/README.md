# Drinking Water Incident Coordinator

This sample evaluates one fixed, fictional water-quality incident. It qualifies
sensor and laboratory records, applies site-specific limits, maps findings to
service zones, and chooses a deterministic next response action using active
advisories, recovery evidence, and alternate-delivery capacity. It is a pure
in-memory planning example; it does not contact an operator, issue an advisory,
or allocate real water.

## Hand-checked fixture result

`fixtures.incident_window()` contains five site/analyte rules, fourteen
measurements, five site-to-zone links, one active advisory, and five zone
supply records. Values are integer milli-units: turbidity is milli-NTU and
E. coli is milli-counts per 100 mL. Times are minutes in one fictional shift.

| Site and analyte | Latest qualified sensor | Latest qualified lab | Finding | Reason |
|---|---:|---:|---|---|
| North intake · turbidity | 2,700 at 600 | 3,100 at 620 | Confirmed | Both meet 2,000; samples are 20 minutes apart within the 45-minute window. |
| North intake · E. coli | 0 at 600 | 1,000 at 620 | Watch | The lab reaches the 1,000 warning limit; the sensor does not. |
| Cedar reservoir · E. coli | 0 at 620 | 0 at 680 | Clear; recovery evidence ready | Both latest sources are at the zero recovery limit, after the minute-515 advisory, and 60 minutes apart. |
| East distribution point · turbidity | 1,500 at 705 | — | Watch | The lab sample collected at 640 arrived at 700, beyond its 30-minute delivery limit. |
| South treatment outlet · E. coli | 4,000 at 710 | 6,000 at 730 | Critical | One qualified latest result reaches the 5,000 critical limit. |

The remaining invalid rows are the North intake sensor backfill (sample 1013,
whose collection time moves backward) and South outlet sensor sample 1014
(whose sequence moves backward). Together with the late distribution lab row,
11 measurements are accepted and 3 rejected.

| Zone | State | Active advisory | Demand / available alternate delivery | Shortfall | Next action |
|---|---|---:|---:|---:|---|
| Northbank | AdvisoryActive | No | 2,000 / 1,500 L/day | 500 L/day | DispatchAlternateSupply |
| Mill Ward | AdvisoryActive | No | 1,200 / 1,500 L/day | 0 | IssueOrMaintainAdvisory |
| Cedar Heights | RecoveryReview | Yes | 800 / 900 L/day | 0 | ReviewRecoveryBeforeLifting |
| Southbank | Critical | No | 1,500 / 1,800 L/day | 0 | ContactHealthAuthority |
| East Ward | Watch | No | 600 / 100 L/day | 500 L/day | CollectConfirmatoryLab |

North intake exposure maps to Northbank and Mill Ward. Confirmed findings
select an `AdvisoryActive` planning state and next action; the table's `Active
advisory` column reports only a matching advisory already present in the
fixture. The reservoir's advisory remains in force while its two post-advisory
recovery results await an authority review. The East Ward capacity gap is
shown for planning, but a
watch-only finding does not dispatch supply before confirmation. Across the
five current findings, the hand-calculated totals are **1 Clear, 2 Watch, 1
Confirmed, and 1 Critical**. One zone is in recovery review; two zones have a
capacity shortfall totaling 1,000 L/day.

## Project structure

- `src/model.sai` defines the bounded data model and report enums.
- `src/fixture_*.sai` provides the fixed sites, thresholds, measurements,
  exposure map, active advisory, and delivery capacities.
- `src/quality.sai` validates site/rule membership, source flags, collection
  and receipt ordering, maximum delivery delay, and per-stream sequence and
  timestamp monotonicity.
- `src/detection.sai` selects the newest accepted result per source and
  applies warning, confirmation-window, and critical thresholds.
- `src/response.sai` checks post-advisory recovery evidence, aggregates exposed
  findings by zone, computes available supply, and selects the next action.
- `src/lib.sai` exposes `assess_fixture` and focused regression assertions.

## Decision rules and precedence

1. A result must reference a known site and threshold, carry the accepted
   source-quality flag, have a receipt time at or after collection, and arrive
   within the rule's 30-minute limit. Within each site/analyte/source stream,
   sequence numbers must increase and collection times must not move
   backward in the supplied order (equal-minute collections are allowed).
   Rejected records do not replace accepted readings.
2. The current sensor and lab readings are the accepted records with the
   greatest collection minute (sequence breaks a same-minute tie). Limits are
   inclusive. A latest result at or above the critical limit is Critical even
   when the other source is lower or missing. Otherwise, both sources must
   meet the warning limit within the confirmation window for Confirmed; one
   elevated source is Watch; no current exceedance is Clear.
3. Any Critical finding makes its exposed zone Critical. Otherwise, a
   Confirmed finding selects an `AdvisoryActive` planning state. Existing active
   advisories stay active when a finding is Watch or Clear. A recovery review
   is eligible only when the current finding is Clear, both latest sources are
   at or below the recovery limit, both were collected after the advisory was
   issued, and their collection times are separated by at least the configured
   interval. Evidence marks a zone RecoveryReview; it never closes an advisory
   by itself.
4. Zone action precedence is Critical notification, an `AdvisoryActive`
   planning state with a delivery shortfall, an `AdvisoryActive` planning
   state with adequate cover, eligible recovery review, Watch confirmation,
   then no action. Available alternate capacity is
   `max(0, daily capacity - committed liters)`; shortfall is
   `max(0, demand - available capacity)`.
5. Every finding maps through an explicit site-to-zone row. Unmapped zones and
   inferred hydraulic connections are outside this fixture.

## Limits

The data is fixed, synthetic, and integer-only. It has no network, file, lab
instrument, water-utility, or dispatch integration. The coordinator does not
model hydraulic flow, mixing, treatment effectiveness, exposure duration,
population vulnerability, legal notice periods, delivery routes, or live
inventory. It evaluates the newest accepted sample in the provided snapshot;
it does not compare sample age with a live clock or infer trends between
snapshots. Delivery capacity is a one-day zone total and is not a vehicle
schedule. A next action is a suggestion for review, not an issued order.

## Review required

- Warning, critical, recovery, confirmation-window, and delivery-delay limits
  are fictional teaching values that differ by site and analyte. A water
  authority and public-health officials must set thresholds and sampling rules
  from applicable standards and site context before any real use.
- One accepted result at the critical limit immediately raises Critical. This
  safety-first choice treats one severe result as enough to escalate even
  before independent confirmation; false positives and confirmation procedures
  remain an authority decision.
- A Watch result does not activate a new advisory or dispatch alternate water.
  This preserves an explicit confirmation step for noncritical readings, but
  can delay action when the only sensor is wrong or the lab is unavailable.
- A record that arrives out of sequence or with a backward collection time is
  excluded instead of being sorted and reconciled. The fixture preserves
  arrival order and surfaces a deterministic quality rejection; a live
  ingestion service would need a late-data and replay policy.
- Recovery requires the newest sensor and lab results both to meet the recovery
  limit after the recorded advisory time and to be separated by the minimum
  interval. The advisory remains active until a human authority reviews the
  evidence; this conservative rule avoids treating two readings as automatic
  authorization to lift a restriction.
- Critical health-authority contact takes precedence over a delivery action.
  Capacity shortfalls remain visible in the zone report and need follow-up
  after escalation; the fixture does not split deliveries or optimize scarce
  vehicles across zones.
- `active_advisory` means that a matching, active notice is present in the
  fixture. A confirmed finding without such a row can still select an advisory
  planning state and action; an actual notice requires an authority decision.
  The sample keeps those facts separate so a recommendation is not reported as
  an already issued public notice.
- Milli-units, resident demand, committed volumes, site/zone links, result
  quality flags, and timestamps are fixture assumptions. They are not
  operational measurements or advice.

## Package and compiler status

The package targets `wasm32-core` and declares no runtime capabilities or
external dependencies. `sai fmt --check` passes for the project. Source
assertions specify expected results, but the current compiler Stage 0 check
and test-analysis gates, along with `@derive`, prevent a verified type-check or
test run. The tables above are hand-calculated from the fixture and rules;
execution is not claimed.
