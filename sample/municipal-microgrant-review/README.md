# Municipal Microgrant Review

This pure SAI library prepares a review plan for one fictional city program that
funds neighborhood cooling, tree canopy, and shared outdoor-space projects. It
starts with a fixed set of seven applications, scores and ranks them using a
published rubric, checks program eligibility in rank order, and awards whole
requests while respecting a cycle budget and a one-award-per-district limit.

The following is the **hand-calculated expected fixture result**. It describes
the return value of `fall_cycle_plan()`; it is not output from a successful
compiler run.

```text
Neighborhood improvement grants — fall cycle
Cycle budget: $100,000; maximum one award per district

Rank  Applicant                   Score   Request     Decision
1     Central Tree Commons           93   $30,000     Awarded $30,000
2     Harbor Youth Garden            90   $18,000     Ineligible — nonprofit registration missing
3     Riverside Neighbors            89   $15,000     Awarded $15,000
4     North Block Trust              86   $35,000     Awarded $35,000
5     Eastside Cooling Co-op         84   $40,000     Not awarded — East district already funded
6     South Market Alliance          79   $25,000     Not awarded — $20,000 remains; requests are not split
7     West Porch Network             75   $20,000     Awarded $20,000

Summary: 7 reviewed, 4 awards, 3 not awarded
Award total: $100,000
Uncommitted budget: $0
```

## Review and allocation rules

- Eligibility checks nonprofit registration, a complete application, service to
  city residents, at least a 20% applicant-funded share of total project cost,
  and a positive request no larger than $50,000. Rubric components above their
  category maximum are rejected as invalid review data.
- Reviewers assign points for neighborhood need (0–40), resident impact
  (0–25), project readiness (0–20), and applicant match (0–15). The total is
  the simple sum of those categories, out of 100. This fixture does not claim
  that the rubric measures impact objectively.
- Applications are ordered by descending score and then ascending application
  ID. Eligibility is checked in that order; an ineligible application uses no
  grant funds or district slot.
- The allocator awards the full requested amount only if the district has not
  received an award and enough budget remains. A request that cannot fit is
  skipped while lower-ranked applications are still considered. The example
  uses a greedy fixed order; it does not search for a combination that
  maximizes the number of awards or guarantees a minimum for every district.

## Project structure and concepts

- `model.sai` defines the application, district, decision reasons, and cycle
  plan records.
- `fixtures.sai` contains seven deliberately unsorted applications and the
  fixed budget and program limits.
- `eligibility.sai` applies the administrative and amount checks.
- `scoring.sai` totals the rubric and resolves rank ties deterministically.
- `allocator.sai` processes ranked applications, tracks the remaining budget
  and awarded districts, and records an explanation for each denial.
- `lib.sai` exposes `fall_cycle_plan()` and contains focused Stage 3 tests for
  budget use, an ineligible high-scoring application, the district limit, and
  the whole-request rule.

The example illustrates bounded records and arrays, module imports, enums,
fixed-data lookups, ordered decision rules, stable ranking, constrained budget
allocation, denial reasons, and batch summaries. It is a pure library: it has
no command-line entry point, file I/O, or external dependency.

## Assumptions and limits

All applicants, scores, and amounts are fictional. The program uses whole
thousands of dollars, treats requested amounts as indivisible, and assumes
unique application IDs. It does not verify legal status, inspect source
documents, calculate project costs, process payments, reserve funds in a live
system, handle appeals or conflicts of interest, or define real city grant
policy. One award per district is a teaching constraint and is not a claim that
this greedy result is geographically fair.

## Package and compiler status

The manifest targets `wasm32-core`, has no external packages, and declares no
runtime capabilities. The test declarations are examples of the expected
checks; they have not been executed. The current compiler reports parser errors
in `allocator.sai` and `scoring.sai` for syntax that its implemented grammar
does not yet accept; it also reports the Stage 0 `sai check` and `@derive`
feature gates, and Stage 3 `sai test` is not implemented. Type correctness and
runtime behavior are not verified; the table above remains a hand-checked
expected result only.
