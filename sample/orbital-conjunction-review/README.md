# Orbital Conjunction Review

Orbital Conjunction Review ranks a fixed set of fictional satellite conjunction
alerts, estimates conservative miss distance from the supplied uncertainty,
and checks whether a proposed maneuver fits the spacecraft's simplified
propellant and timing constraints. It emits operator-readable recommendations
in priority order and treats feasible recommendations as accepted for
portfolio budgeting, reserving each spacecraft's propellant budget as they
are processed.

## Hand-checked fixture decisions

`src/data.sai` deliberately stores alerts in a different order from review
priority. The review order is risk band, conservative miss bound, time to
closest approach (TCA), then alert ID. Conservative miss bound is the reported
miss distance minus uncertainty, floored at zero.

| Alert | Spacecraft | Conservative bound | TCA | Risk | Recommendation |
|---|---|---:|---:|---|---|
| #101 / DR-77 | Aster-1 | 400 m | 60 h | Critical | Monitor; an 80 mm/s burn would leave 20 mm/s, below the 60 mm/s reserve |
| #209 / FRAG-22 | Lark-4 | 950 m | 20 h | Critical | Recommend an 80 mm/s burn for operator approval; 140 mm/s remains |
| #312 / DEB-03 | Kestrel-9 | 1,350 m | 40 h | Critical | Monitor; its approved maneuver window is closed |
| #103 / DR-91 | Aster-1 | 1,800 m | 16 h | Critical | Monitor; 16 h is below its 48 h minimum lead time |
| #207 / OBJ-5 | Lark-4 | 2,200 m | 50 h | Critical | Recommend an 80 mm/s burn for operator approval; retain exactly 60 mm/s |
| #208 / OBJ-6 | Lark-4 | 3,600 m | 36 h | Elevated | Monitor; below the automatic maneuver band |
| #311 / DEB-14 | Kestrel-9 | 4,300 m | 92 h | Elevated | Monitor the next state-vector update |
| #318 / UNKNOWN-9 | Kestrel-9 | 2,200 m | 30 h | Watch | Monitor and request better tracking; confidence is low |
| #314 / OLD-44 | Kestrel-9 | 12,000 m | 180 h | Routine | Hold; outside the 168 h review horizon |

The hand-checked portfolio result is five critical, two elevated, one watch,
and one routine alert. The recommendations are two maneuver proposals, six
monitor actions, and one hold. The two proposed Lark-4 burns reserve 160 mm/s
of its 220 mm/s modeled budget and leave the protected 60 mm/s reserve intact.
The Aster-1 and Kestrel-9 critical alerts remain visible for operator review
instead of being silently discarded when their constraints block a proposal.

The expected report begins with operator-facing sentences such as:

```text
#101 Aster-1 / DR-77: CRITICAL — MONITOR. Do not queue the 80 mm/s burn: it would leave only 20 mm/s against a 60 mm/s reserve. Escalate for operator review. Conservative miss bound 400 m; TCA in 60 h; confidence 9700 bps.
#209 Lark-4 / FRAG-22: CRITICAL — MANEUVER. Recommend an 80 mm/s burn for operator approval; 140 mm/s would remain and the 60 mm/s reserve would be protected. Conservative miss bound 950 m; TCA in 20 h; confidence 9800 bps.
#312 Kestrel-9 / DEB-03: CRITICAL — MONITOR. No approved maneuver window is open; keep monitoring and escalate to the flight dynamics operator. Conservative miss bound 1350 m; TCA in 40 h; confidence 9100 bps.
```

## Modules

- `model.sai` defines spacecraft constraints, alerts, risk bands, assessments,
  and the review plan.
- `data.sai` supplies three fictional spacecraft and nine shuffled alerts.
- `policy.sai` computes conservative miss bounds, risk bands, and stable
  review precedence.
- `review.sai` evaluates constraints, recommends feasible critical burns, and
  reserves modeled propellant budgets across alerts.
- `report.sai` prints the ordered findings and portfolio totals.
- `lib.sai` exposes the pure fixture review and records expected regression
  cases.

## Review required

- The 2,500 m critical corridor, 6,000 m elevated/watch corridor, 7,000 bps
  Watch-to-Elevated confidence cutoff, 8,000 bps critical confidence gate,
  72 h critical window, and 168 h review horizon are teaching thresholds.
  Within both the 6,000 m corridor and 168 h review horizon, confidence below
  7,000 bps yields Watch; confidence at or above 7,000 bps can yield Elevated.
  Critical also requires a bound at or below 2,500 m, confidence at or above
  8,000 bps, and TCA within 72 h. Alerts beyond 168 h are Routine. A flight
  dynamics team must replace these values with its validated conjunction
  assessment and escalation policy.
- The policy subtracts the full uncertainty radius from miss distance and
  floors at zero. It does not interpret covariance, encounter geometry, or a
  collision probability; operators must decide whether this deliberately
  conservative bound is useful for their review process.
- Every critical alert proposes a fixed 80 mm/s burn. The fixture treats this
  as a budgeting placeholder, not an orbit solution or a command. An operator
  must design, approve, coordinate, and validate any real maneuver against all
  mission constraints.
- Maneuver blockers are reported in source order: closed approved window,
  insufficient minimum lead time, single-burn limit, then protected propellant
  reserve. The first blocker wins even if a later check would also fail. For
  example, #312 reports its closed window before its propellant shortfall, and
  #103 reports its lead-time shortfall before its propellant shortfall.
- The alerts are processed serially in the published priority order.
  Recommended burns reserve an equivalent delta-v budget in this simulation;
  blocked proposals do not reserve fuel. A real planner must account for
  maneuver interactions, state-vector updates, fuel uncertainty, and
  concurrent operator decisions.
- When a critical alert is blocked, the sample recommends monitoring and
  escalation. It never turns an infeasible maneuver into an automatic hold or
  dismisses the alert.
- All objects, tracking values, windows, and fuel budgets are fictional and
  hand-entered. This is not flight software or operational conjunction advice.

## Limits and compiler status

The example is a deterministic, in-memory `wasm32-wasi` CLI package. Its
expected decisions and report sentences are checked by hand against the
fixtures.
`sai fmt --check` validates source syntax and canonical formatting, but does
not prove name resolution, type correctness, or runtime behavior. The current
`sai check` reports the Stage 0 type-check gate, the Stage 3 dependency gate
for `sai:process`, and the unsupported `@derive` attributes. Source-level tests
and the CLI therefore cannot yet be semantically checked or run.
