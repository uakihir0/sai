# Facility GHG Inventory Auditor

This fixed-fixture library reviews one organization's annual activity register
across three sites. It checks the reporting boundary, period, activity and
evidence identity, then selects approved emission factors by source, unit,
scope, accounting method, and validity year. Accepted activity is multiplied
with checked integer arithmetic and accumulated into separate Scope 1, Scope 2,
and Scope 3 totals.

Scope 2 electricity is reported twice: once with a location-based grid factor
and once with a market-based contractual factor. These are alternative
accounting views. The two totals below never add both Scope 2 results together.
All names, quantities, factor rates, source-document references, boundary
decisions, and outputs are fictional teaching data.

## Hand-checked 2025 result

| Activity | Scope | Input | Factor (gCO2e/unit) | Result | Finding |
|---|---|---:|---:|---:|---|
| Grid electricity | 2 | 12,000 kWh | Location 410; market 22 | Location 4,920,000g; market 264,000g | Accepted |
| Natural gas | 1 | 600 therms | 5,300 | 3,180,000g | Accepted |
| Fleet diesel | 1 | 1,200 L | 2,680 | 3,216,000g | Accepted |
| Passenger flight | 3 | 4,000 passenger-km | 158 | 632,000g | Accepted |
| Refrigerant leak | 1 | 4 kg | 1,430, expired after 2024 | — | Expired factor |
| Landfill waste | 3 | 900 kg | — | — | Missing factor |
| Grid electricity | 2 | 0 kWh | — | — | Zero quantity |
| Natural gas at excluded site | 1 | 20 therms | 5,300 | — | Outside boundary |
| Fleet diesel | 1 | 5 L, day 366 | 2,680 | — | Outside reporting period |
| Two records with activity ID 1010 | 1 | 10 and 11 therms | 5,300 | — | Both held as duplicate IDs |
| Two records with evidence ID 8012 | 1 | 2 and 3 L | 2,680 | — | Both held as duplicate evidence |
| Natural gas | 1 | 1 therm | 5,300 | — | Missing activity evidence |

Four of the 14 reviewed rows are accepted and ten are held. The hand-checked
totals are:

- Scope 1: 6,396,000g CO2e.
- Scope 2 location-based: 4,920,000g CO2e.
- Scope 2 market-based: 264,000g CO2e.
- Scope 3: 632,000g CO2e.
- Location-based inventory total: 11,948,000g CO2e.
- Market-based inventory total: 7,292,000g CO2e.

For either inventory total, Scope 1 and Scope 3 are included with exactly one
Scope 2 result. The code uses integer grams and rejects multiplication or
aggregate addition that would exceed `U32`.

## Audit flow and module map

1. `data.sai` supplies three sites, 14 activity rows, and six factor records.
2. `validation.sai` rejects duplicate activity IDs and evidence references,
   missing evidence, incomplete or duplicated site-register identities,
   excluded sites, activity outside the reporting year/day window, and zero
   quantities. Both rows in an activity identity collision are held.
3. `registry.sai` maps sources to scopes and selects exactly one factor by
   source, unit, scope, method, and inclusive validity year. Non-overlapping
   historical versions are supported; multiple versions covering the same
   report year are ambiguous, and a gap between expired and future versions is
   reported. Sparse factor rows, zero or duplicate factor IDs, unsupported
   units, unapproved records, missing source-document references, and zero
   rates fail closed.
4. `calculator.sai` requires both Scope 2 methods before accepting an
   electricity row. It retains each selected factor ID and computes grams with
   guarded multiplication.
5. `audit.sai` preflights array counts and the complete site/factor catalogs
   before producing any row decisions. A malformed catalog returns an
   inventory-level issue and an empty report. Otherwise it preserves fixture
   order and accumulates scope and alternate inventory totals only when every
   addition is safe.
6. `lib.sai` exposes `fixture_report()` and embeds expected-result assertions.

## Package and compiler limits

This is a dependency-free, in-memory `wasm32-core` package. It uses bounded
arrays and has no file, network, database, reporting, or persistent-state
effects. The fixed fixture uses an inclusive 365-day calendar for 2025. The
assertions describe intended outcomes; they are not a claim that the sample
has been executed. The current compiler can format the sources, but the Stage
0 checker and `@derive` support are incomplete, and the Stage 3 test runner is
not implemented. Type checking and test execution therefore remain
unverified. The embedded assertions include sparse site/factor catalogs and
unreferenced duplicate site identities, cross-scope factor-ID collisions, and
both per-activity multiplication overflow and aggregate overflow cases.

The model has one reported quantity per activity row and selects one valid
factor per source, unit, scope, method, and report year. It does not implement
inventory standards, organizational consolidation alternatives, multiple
Scope 2 instruments, supplier-specific data quality, biogenic CO2, offsets,
avoided emissions, removals, uncertainty, market instrument quality checks,
factor conversions, or full Scope 3 category coverage. It is an illustrative
workflow, not an assured inventory, accounting advice, or regulatory filing.

## Review required

- **Boundary:** Site membership is a fixed Boolean input and the fixture uses
  one boundary choice for all activities at each site. A real organization
  must approve its consolidation approach, ownership/control rules, leases,
  acquisitions, divestitures, and treatment of joint ventures. Duplicate,
  missing, or zero site IDs fail the whole input preflight, including when
  no activity references the malformed site.
- **Scope mapping:** Electricity is Scope 2; natural gas, fleet diesel, and
  refrigerant leakage are Scope 1; passenger flights and landfill waste are
  Scope 3. These mappings are teaching assumptions. Review source-specific
  categorization and the applicable Scope 3 category before any real use.
- **Scope 2 alternatives:** The inventory exposes location-based and
  market-based totals separately and does not sum them. The sample accepts a
  market factor when its document reference is nonzero and the record is
  marked approved; this does not authenticate contractual instruments or
  establish their quality, vintage, market boundary, or retirement.
- **Factors and periods:** Factor validity years include both endpoints.
  Activity periods must be within days 1–365 of the report year; no leap day,
  time zone, partial-period normalization, or restatement policy is modeled.
  Non-overlapping factor versions can coexist, but exactly one version must
  cover a report year; more than one version covering that report year is
  ambiguous, and a gap produces no selection. Declared factor rows must
  be present and have globally unique, nonzero IDs before any activity is
  reviewed. The rates and integer gram units are fictional and must not be
  substituted for official factors.
- **Evidence and duplicate handling:** Duplicate activity IDs or activity
  evidence references hold every colliding row, rather than selecting a
  winner. Missing activity evidence and missing factor documents also hold.
  A real control owner should confirm whether evidence references are unique
  globally, by source system, or only within a reporting period.
- **Rounding and completeness:** Multiplication is exact integer arithmetic
  in grams; the sample does not round, quantify uncertainty, or claim
  completeness. A production inventory needs controlled conversion and
  rounding rules, data-quality scoring, review sign-off, and coverage tests.
