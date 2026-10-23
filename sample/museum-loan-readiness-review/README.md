# Museum Loan Readiness Review

This fictional museum reviews ten incoming object loans for one exhibition.
The report checks accession identity, lender consent, loan-file completeness,
condition, insurance, shipping crates, gallery humidity, and limited display
capacity. It produces review decisions only; it does not approve a real loan or
arrange transport.

The following is a hand-calculated expected result from the fixed fixture, not
program output. Requests are ordered by ascending priority number and then
request ID.

```text
Museum loan readiness — North Fen Museum
Reviewed: 10   Accepted: 2   Held: 3   Deferred: 3   Rejected: 2

Request  Accession  Decision
4107     7101       Reject — accession appears in more than one request
4102     7102       Hold — insurance is below appraised value
4103     7103       Hold — loan file is incomplete
4106     7106       Accept — climate case and display slot reserved
4101     7101       Reject — accession appears in more than one request
4104     7104       Defer — required shipping crate is unavailable
4108     7108       Accept — one display slot reserved
4109     7109       Defer — no display slot remains
4110     7110       Defer — gallery humidity is outside the loan range
4105     7105       Hold — object condition is unstable

Summary: 2 accepted, 3 held, 3 deferred, 2 rejected
```

## Review sequence

- Before evaluating individual requests, the reviewer counts each accession
  across the complete batch. Every request using a repeated accession is
  rejected, so array order cannot let one duplicate reserve space first.
- Other checks run in this order: lender consent, complete loan file, stable
  condition, insurance at least equal to appraised value, a suitable available
  crate, and (for climate-sensitive objects) gallery humidity inside the
  lender's range. A failed check holds or defers the request without using
  display capacity.
- Ready requests share two display slots and one climate case. Priority 1 is
  reviewed before priority 2, then priority 3; request ID breaks ties. The
  accepted field journal (4106) uses the climate case. The measuring prism
  (4108) uses the second display slot. Request 4109 is then deferred because
  the gallery is full.
- The gallery is at 50% relative humidity. Request 4110 requires 55–60%, so it
  is deferred even though its shipping crate is adequate. Request 4104 passes
  its 45–55% humidity requirement, but has no crate rated for its handling
  needs.
- A `Monitor` condition may be accepted after other checks. `Unstable` condition
  is held. This simplified fixture does not create or model conservation
  treatment plans.

## Modules and concepts

- `model.sai` defines the request, gallery, decision, reason, and report types.
- `data.sai` contains the fixed gallery and ten fictional requests.
- `review.sai` detects batch-wide accession collisions, applies ordered
  readiness checks, sorts requests deterministically, and reserves capacity.
- `lib.sai` exposes `sample_report()` and includes example regression tests.

This is an in-memory example using fixed arrays. The reviewer fails closed with
an empty report marked invalid if `request_count` exceeds ten or any slot in the
counted prefix is empty. Request IDs must be unique for deterministic ordering.
The accession check deliberately rejects all members of a duplicate group
instead of guessing which record is authoritative. Dollar values, condition
categories, crate ratings, and environmental ranges are illustrative; museum
lenders, conservators, insurers, and transport professionals determine real
requirements.

## Compiler status

The manifest targets `wasm32-core` and declares no external packages or
capabilities. `sai fmt --check` is the available source-format check. The
current Stage 0 checker and `@derive` support are incomplete, and Stage 3
`sai test` is not implemented, so this fixture and its expected report have not
been semantically compiled or executed. The table above is a manual
reconciliation, not a successful program run.
