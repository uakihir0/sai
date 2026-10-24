# Construction Permit Review

Construction Permit Review screens a small, fictional batch of building permit
applications. It reports required evidence, missing documents, zoning and
specialist review triggers, and one deterministic status for each application.
The sample only routes records for further review; it never approves a permit.

## Hand-checked example result

`example_report()` returns one review per application, in fixture order:

| ID | Application | Evidence required / missing | Review flags | Status |
|---:|---|---:|---|---|
| 101 | Willow Row House | 2 / 0 | — | Staff plan review |
| 102 | Harbor Corner Cafe | 3 / 2 | Zoning, accessibility, environment | Incomplete and specialist review |
| 103 | North Foundry Addition | 2 / 0 | Zoning | Zoning review |
| 104 | Cedar Court Flats | 3 / 0 | Accessibility | Accessibility review |
| 105 | Creekside Workshop | 4 / 0 | Environment | Environmental review |
| 106 | Maple Loft Conversion | 2 / 1 | — | Incomplete evidence |
| 107 | Market Street Hall | 3 / 0 | Zoning, environment | Multi-discipline review |
| 108 | Empty Parcel Datum | 2 / 0 | Data correction | Data correction required |

There is one application in each of the eight status categories. Seven
applications need follow-up. The first application is complete under the sample
rules and enters staff plan review; this is not permit approval.

## Review flow

1. `fixtures.sai` provides eight deliberately varied applications: an ordinary
   house, a public cafe with evidence gaps and several review triggers, a
   use/zone conflict, a multi-unit building, a flood-screened renovation, an
   incomplete renovation, a combined zoning/environmental case, and invalid
   site measurements.
2. `policy.sai` decides which documents are required and which zoning or
   specialist reviews are triggered. It also identifies missing parcel
   measurements.
3. `evaluator.sai` records every requirement and missing item, preserves
   independent review flags, and assigns one status using the documented
   priority.
4. `report.sai` evaluates all eight applications in fixture order and produces
   mutually exclusive status counts plus a total follow-up count.
5. `lib.sai` exposes `example_report()` and regression assertions for the
   fixture results and multi-review cases.

The sample does not sort by name, infer missing data, or query a parcel system.
Given the same fixture, application order, required evidence, flags, and status
are deterministic.

## Sample screening rules

- Every application requires a site plan. New construction and structural
  renovation require structural drawings. Demolition requires a demolition
  plan.
- An accessibility plan and accessibility specialist route are triggered by a
  public entry or four or more dwelling units.
- A drainage plan is triggered by a flood-screening flag or soil disturbance
  greater than 500 square metres. An environmental note is required for a
  flood-screened parcel or a site marked near a waterway.
- Environmental specialist review is triggered by the flood-screening flag, a
  near-waterway flag, or soil disturbance greater than 500 square metres.
- Dwelling use is accepted by the sample in residential and mixed-use zones;
  retail or food service in mixed-use and commercial zones; and light
  industrial use in industrial zones. A use mismatch, insufficient supplied
  setback, or footprint larger than the supplied lot triggers zoning review.
  Demolition is not screened against proposed-use zoning or setback rules.
- A zero lot area or zero footprint triggers data correction. This status has
  first priority so invalid measurements are visible even if another rule
  would otherwise route the application.
- After data correction, missing required evidence takes priority. If any
  specialist route is also active, the status is “Incomplete and specialist
  review.” With complete evidence, multiple specialist flags become “Multi-
  discipline review”; one flag selects that discipline. No flags and no missing
  evidence yields “Staff plan review.”

Evidence requirements and review triggers are also returned as individual
booleans. A single status does not discard concurrent zoning, accessibility,
or environmental flags.

## Project structure and concepts

- `model.sai` defines permit inputs, screening statuses, per-application review
  results, and the batch report.
- `fixtures.sai` contains eight fictional application records with varied
  evidence and parcel conditions.
- `policy.sai` keeps sample thresholds, evidence requirements, zone/use
  compatibility, and independent specialist triggers in one module.
- `evaluator.sai` calculates required and missing evidence, review flags, and
  status priority for one application.
- `report.sai` preserves fixture order and counts each final status.
- `lib.sai` exposes the example report and source-level test declarations.

## Limits

This is a bounded, in-memory example with eight fixed records and whole-number
square-metre/millimetre fields. It assumes all nonzero measurements and supplied
zone, flood-screening, waterway, and soil-disturbance flags have already been
entered correctly. The sample does not geocode a parcel, load an adopted zoning
map, derive setbacks from law, inspect drawings, verify accessibility design,
measure environmental impacts, resolve variances, or communicate with an
applicant. It does not cover every permit type or local rule. It is not legal,
engineering, accessibility, environmental, or permitting advice.

## Review required

- The zone/use matrix is invented for this teaching sample. In particular, the
  three proposed-use categories and their zone compatibility must not be
  treated as a real zoning code.
- The four-unit accessibility trigger, public-entry trigger, 500-square-metre
  soil threshold, flood-screening flag, and waterway flag are simplified
  routing assumptions. A jurisdiction must replace them with its applicable
  laws, maps, definitions, and review process.
- The setback values are supplied on each fixture; the sample does not derive
  them from zoning. A supplied setback below its supplied minimum is only a
  reason to route the record for review, not a finding of noncompliance.
- A zero lot area or footprint is treated as a data correction issue. The
  sample does not define tolerances, units conversion, or treatment of
  measurement uncertainty.
- Status priority makes incomplete evidence visible before complete specialist
  routing, while retaining all specialist flags. A real intake team should
  confirm whether it wants this triage order or concurrent work queues.
- “Staff plan review” means only that this sample found no configured evidence
  gap, specialist trigger, or invalid measurement. It is not a finding that an
  application is complete under law or that a permit should be issued.

## Compiler and test status

The manifest targets `wasm32-core` and declares no external dependencies or
runtime capabilities. The source illustrates modules, records, enums, fixed
arrays, deterministic policy evaluation, and source-level assertions. The
repository's current compiler does not yet implement all `sai check` and
`sai test` gates needed to validate this sample end to end. The table and test
expectations are hand-checked against the fixture and policy; they are not
claimed as compiler-generated output.
