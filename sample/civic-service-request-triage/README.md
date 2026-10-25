# Civic Service Request Triage

This fixed-fixture library turns fictional resident reports into an auditable
municipal intake queue. It checks intake identity and time bounds, keeps
non-city assets out of city dispatch, sends unknown classifications to review,
links same-day reports for one incident, and routes supported cases with a
priority and response target. It recommends ownership and timing; it does not
contact a department or dispatch a crew.

## Hand-checked report

`report::render_report()` is intended to produce a readable report like this:

```text
Civic service request triage
Duplicate key: service + municipal asset location + intake day; earliest report is retained
Response targets: critical 30 min; high 240 min; standard 2,880 min

#8401 sidewalk access — Juniper & 3rd curb ramp; routed to Access & Mobility; HIGH; target minute 690; 2 report(s)
#8402 traffic signal — Market Square crossing signal; routed to Traffic Operations; CRITICAL; target minute 490; 2 report(s)
#8403 Juniper & 3rd curb ramp — duplicate report linked to #8401
#8404 missed collection — Cedar Court missed organics pickup; routed to Sanitation; STANDARD; target minute 3390; 1 report(s)
#8405 Lakeside private-park path light — outside municipal responsibility; jurisdiction referral required
#8406 water leak — Canal Road water-main leak; routed to Water Utility; CRITICAL; target minute 560; 1 report(s)
#8407 park hazard — Maple Park accessible path; routed to Parks; HIGH; target minute 1730; 1 report(s)
#8408 Market Square crossing signal — duplicate report linked to #8402
#8409 traffic signal — Market Square crossing signal; routed to Traffic Operations; STANDARD; target minute 4390; 1 report(s)
#8410 North Canal retaining wall — service classification review; no dispatch
#8411 missed collection — Pine Terrace missed refuse pickup; routed to Sanitation; STANDARD; target minute 4375; 1 report(s)

Summary: 7 routed; 2 duplicate reports linked; 1 jurisdiction holds; 1 classification reviews; 0 invalid records; 2 critical cases
```

Times are absolute minutes from the beginning of a fictional seven-day intake
fixture. The output retains submission order for auditability. The response
target is a policy deadline for first response, not a promised repair time.

## Workflow and concepts

- `model.sai` defines request identity, service categories, triage decisions,
  owning departments, urgency, and report rows.
- `data.sai` supplies eleven fictional, deliberately mixed service reports.
- `triage.sai` validates IDs and bounded timestamps, links same-day incident
  reports, assigns supported service categories, and calculates targets.
- `report.sai` renders routing decisions and holds as a user-readable queue.
- `lib.sai` exposes the structured report and regression cases.

The example covers records, enums, fixed arrays, module imports, loops,
`Option`, deterministic duplicate canonicalization, intake validation,
category routing, priority classification, bounded SLA arithmetic, and
explainable review outcomes. It stores only fictional numeric asset-location
codes and no resident names, contact details, or free-text reports.

## Limits

The fixture is a triage demonstration, not a 311 integration. It does not
verify resident identity, geocode an address, prove ownership, notify residents,
contact another jurisdiction, schedule work, check staffing or spare parts,
parse attachments, preserve an operational audit log, or resolve a service
request. Duplicate detection is intentionally strict: only the same known
service category, municipal asset location, and reported intake day are linked.
The first reported time wins; equal times use the lower request ID. Cases
outside city responsibility are held for a jurisdiction referral, whose
recipient is not supplied by this example.

## Review required

- Confirm that only intake staff may set `immediate_hazard` and
  `accessible_route_blocked`, and define escalation for unverified safety
  claims. An immediate hazard receives a 30-minute first-response target;
  blocked accessible routes receive 240 minutes; other accepted reports
  receive 2,880 minutes. These thresholds are illustrative, not adopted city
  service standards.
- Confirm the exact duplicate key and the rule to retain the earliest report,
  using the lower ID only for a same-minute tie. A real system may need
  distance-based matching, reopen windows, privacy constraints, or human
  confirmation before merging.
- Confirm that unknown categories and invalid identities are held without
  dispatch, and that out-of-jurisdiction requests receive a hold rather than
  an automatic transfer. This sample has no verified receiving authority or
  contact endpoint.
- Confirm the seven-day timestamp bound, the absolute-minute representation,
  and whether response targets begin at report receipt. The fixture contains
  only valid unique IDs except in explicit regression cases; production input
  must authenticate and validate its source.

## Package and compiler status

This is a dependency-free `wasm32-core` library package. The expected report is
hand-checked against the fixture; it is not claimed as compiler output. The
sample passed `sai fmt --check`. The current implementation stages do not yet
complete `sai check` for Stage 0 and `@derive`, or `sai test` for Stage 3, so
these embedded assertions have not been executed and semantic correctness and
runtime behavior remain unverified.
