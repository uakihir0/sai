# Affordable Housing Allocation

Cedar Grove Affordable Homes prepares a fixed, auditable offer plan for four
fictional rental homes and six fictional applicants. It checks household-size
limits, each home's income ceiling, a 30% gross monthly rent-to-income cap,
step-free and wide-door requirements, and each applicant's ranked area
preferences.

```text
affordable-housing-allocation allocations
Cedar Grove Affordable Homes — rental offer plan
Tie-break: preferred area, earliest application day, then ascending application ID
Unit #201 18 Pine Court, Apt 1 (North, 1 BR, $900/month): #1001 Aki Sato — area preference 1, household 2
Unit #202 44 Market Street, Apt 3 (Central, 2 BR, $1200/month): #1002 Bo Tanaka — area preference 1, household 4
Unit #203 7 Orchard Row, Apt 2 (South, 2 BR, $800/month): #1004 Darius Okafor — area preference 2, household 3
Unit #204 9 Orchard Row, Apt 0 (South, 0 BR, $500/month): #1003 Chika Mori — area preference 1, household 1
Offers made in this plan: 4
```

The planner visits homes in their fixture release order. For each home it
selects an eligible applicant who has not already received an offer. The
selected applicant is removed from consideration for later homes, so one
application cannot receive two offers. Lower area-preference rank wins first;
ties use earlier submission day and then lower application ID. Rent is
considered affordable when monthly rent is at most 30% of the stated gross
monthly income. Applicants may still be offered a home outside their first
and second area choices if it is otherwise eligible.

The waitlist command keeps every unoffered application visible and distinguishes
applicants who qualified for a listed home from those with no match under the
fixture's rules. In this fixture, Emi qualified for the accessible studio but
was not selected there; Farah's income exceeds the ceiling of every listed
home.

## Commands

- `allocations` prints each home and its offer, or marks it vacant.
- `waitlist` prints unoffered applicants and a concise match status.
- `summary` counts offers and waitlisted applicants and states the main cap.

## Project structure

- `model.sai` defines applicants, homes, areas, decisions, and the plan.
- `data.sai` contains the fictional applicant and home fixture in release order.
- `eligibility.sai` applies household-size, income, rent, and accessibility
  rules.
- `allocator.sai` chooses one unassigned applicant per home using stable
  preference and application-ID tie-breaking.
- `report.sai` prints offers, waitlist status, and a summary; `lib.sai`
  dispatches the command and declares regression assertions for the allocation
  and waitlist fixture.

The example illustrates multi-module SAI packages, records and enums, fixed
arrays, nested loops, eligibility policy, deterministic ranking, a
single-offer invariant, and a waitlist report.

## Review required

- The household occupancy ceilings, home income ceilings, 30% rent cap, and
  accessibility attributes are fictional teaching rules. A real program must
  use its jurisdiction's approved occupancy standard, subsidy definitions,
  income-calculation method, reasonable-accommodation process, and appeal
  policy.
- The fixture ranks preferred areas ahead of application age and ID, and
  processes homes in listed release order. This is deterministic and easy to
  audit, but it is not a globally optimized or legally validated matching
  policy. A housing administrator should approve the priorities and confirm
  how preferences interact with waitlist tenure before using a real dataset.
- The sample models step-free entry and wide doorways as binary features. It
  does not assess a person's complete accommodation request, offer a review
  process, or decide whether an exception is required.
- The rent test compares cents using integer arithmetic and assumes positive
  gross monthly income. The fixture is small enough to avoid integer overflow;
  production inputs would need validated ranges and an explicit treatment of
  utilities, deductions, variable income, and rounding.
- The waitlist is recalculated from the entire fixed fixture each time. There
  is no persistence, applicant notification, offer acceptance/decline,
  deadline, fraud check, household update, or concurrent leasing workflow.

## Package and compiler status

The manifest targets `wasm32-wasi` and uses the `sai:process` command-line
entry-point shape. The `sai:process` Git URL is currently unavailable, so
package resolution needs a matching toolchain bundle index or a published
source repository. `sai fmt --check` passes. An offline `sai check` reports the
current Stage 0 check gate, the Stage 3 `sai:process` dependency gate, and the
`@derive` feature gate, so it cannot verify the complete program. No successful
check, build, or run is claimed. The output above is hand-checked against the
fixed fixture.
