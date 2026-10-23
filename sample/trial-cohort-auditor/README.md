# Trial Cohort Auditor

This fixed, fictional Wave A study log exercises two linked review passes:
participant screening and longitudinal visit-data quality. Screening decisions
control which participant IDs enter the visit audit, so an orphan visit cannot
silently inflate the enrolled cohort. The sample uses no personal data,
external services, files, or persistent state.

The table below is the **hand-checked expected fixture report**, not program
output. The source is intentionally unsorted; report rows are ordered by
participant ID, visit, and finding kind so repeated runs produce the same
result.

```text
Study cohort data quality audit — Wave A (expected fixture result)

Screening log: 7 records; 2 enrolled; 5 not enrolled
  P-102: consent declined
  P-103: age 17 is outside the illustrative 18–70 range
  P-104: duplicate participant ID in records 7004 and 7005
  P-105: exclusion criterion recorded
  P-101 and P-106: enrolled

Scheduled visits for enrolled participants: 6
  Observed enrolled visit records: 5
  Ineligible-participant visit records: 1

Review findings (4):
  P-104 baseline: visit record belongs to a participant excluded at screening
  P-106 week 4: day 35 is outside the day 26–30 visit window
  P-106 week 4: serious-event report on day 37 missed the day-35 fixture deadline
  P-106 week 12: required visit is missing (window day 82–86)

Summary: 1 missing visit, 1 out-of-window visit, 1 late safety report,
         1 visit record for an ineligible participant; 1 serious event observed
```

## Fixture policy and assumptions

- The eligibility order is duplicate participant ID, signed consent, age from
  18 through 70 inclusive, then the fixture's exclusion flag. If an ID appears
  more than once, **every** matching screening row is excluded; the first row
  does not win based on input order.
- Enrolled participants require baseline on day 0, week 4 on days 26–30, and
  week 12 on days 82–86. Both endpoints are inclusive.
- A serious-event report is considered on time by this fixture when it is
  recorded no later than one calendar day after onset. This is an illustrative
  protocol setting, not a statement of any regulator's reporting rule.
- Visits for participants not enrolled are reported separately and do not
  count as enrolled observations or safety events. Findings are sorted by
  participant ID, visit order, then finding kind.
- The data model has fixed bounds of seven screening rows and six visit rows.
  Screening record IDs are unique in the fixture. Duplicate visit records for
  enrolled participants are flagged but not reconciled; records for ineligible
  participants are reported individually. The sample does not model multiple
  sites, withdrawals, rescreening, blinded randomization, clinical
  interpretation, or regulatory submissions. Callers must keep count fields
  within the fixed array bounds and populate every slot before each count.

## Project structure and concepts

- `model.sai` defines screening records, visit observations, report rows, and
  typed finding categories.
- `data.sai` supplies seven deliberately unsorted screening records and six
  visit records, including duplicate IDs, an out-of-window visit, an incomplete
  schedule, and a late safety report.
- `screening.sai` applies eligibility rules, rejects all duplicate IDs, counts
  decisions, and sorts the screening report deterministically.
- `visit_audit.sai` checks cohort membership, visit windows, required visits,
  and safety-report deadlines, then sorts findings deterministically.
- `lib.sai` exposes `audit(dataset)` and the fixed `sample_audit()` entry point.
  Source-level tests describe expected counts and ordering.

## Package and compiler status

The package targets `wasm32-core`, has no external dependencies, and declares no
runtime capabilities. The sample's test declarations have not been executed:
Stage 3 `sai test` is not implemented, and current Stage 0 `sai check` is gated
on `@derive`. `sai fmt --check sample` can check parsing and formatting, but it
does not establish type correctness or runtime behavior. The report above is a
manual fixture expectation only.

This is an educational data-quality example, not medical decision support. All
participant IDs, records, eligibility rules, visit windows, and deadlines are
invented for the fixture; they do not prescribe clinical care or demonstrate
regulatory compliance.
