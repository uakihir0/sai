# Digital Evidence Custody Auditor

This fixed-fixture library audits a fictional digital evidence register for a
single incident day. It checks whether the source custody stream names known
evidence and authorized actors, preserves per-evidence timestamp order, tracks
seal and storage transitions, limits access to approved purposes, and records
the required final disposition. It returns a typed report containing both
findings and a separate custody timeline.

## Scenario and fixture

The register contains four items: a field laptop to retain, a USB drive to
return, a damaged phone to destroy after review, and an encrypted disk to
retain. Four actors represent an intake custodian, a forensic analyst, a
transport custodian, and an evidence supervisor. Actor permissions are
explicit action flags plus a fixed list of allowed access purposes.

The 20-event fixture is deliberately unsorted across evidence streams. The USB
drive's seal-break event is backdated after a later storage move in the source
stream. The analyst also attempts a legal-review access that is outside their
purpose grant. The phone reaches a destruction event while its seal remains
open, and the encrypted disk has no custody events or final disposition.

## Algorithm

1. Scan actor and evidence records against earlier rows and report one
   duplicate-ID finding for each later duplicate row.
2. Traverse custody events in their supplied order. This order is the source
   event stream and is used to check each item's timestamp high-water mark and
   replay its collection, seal, storage, access, and disposition state.
3. Check each event's actor against the action grant. For access events, also
   require a purpose and check it against that actor's purpose list.
4. Treat disposition as terminal, compare it to the evidence register's
   required disposition, and report evidence with no disposition event.
5. Insert findings by evidence ID, optional actor ID, event minute, event ID,
   then finding code. Independently insert all events into the timeline by
   minute, evidence ID, then event ID. Equal sort keys preserve their fixture
   order.

The event timestamp is an integer minute after 09:00 on the fictional audit
date, so the sample avoids implying a timezone or calendar implementation.

## Expected output

`audit.audit_fixture()` returns four findings in this order:

| Evidence | Event | Finding |
|---:|---:|---|
| 4102 | 9013 | `NonMonotoneTimestamp` |
| 4102 | 9014 | `UnauthorizedAccessPurpose` |
| 4103 | 9024 | `DispositionWithOpenSeal` |
| 4104 | — | `MissingFinalDisposition` |

The returned timeline has all 20 source events sorted chronologically. At
minute 40, event 9004 for evidence 4101 precedes event 9022 for evidence 4103;
at minute 41, event 9005 precedes event 9023; at minute 42, event 9006
precedes event 9024. The backdated USB event 9013 appears at minute 50 in the
timeline even though it follows the minute-55 event in its source stream. The
timestamp finding preserves that discrepancy instead of silently rewriting
the audit input.

## Source map

- `src/model.sai` defines evidence, actor, event, finding, and report types.
- `src/fixtures.sai` supplies the case register, action grants, and custody
  events, including the intentional timestamp, access-purpose, seal, and
  missing-disposition exceptions.
- `src/policy.sai` resolves IDs and checks action and purpose grants.
- `src/audit.sai` validates source-order timestamps and replays the custody
  state machine, then emits a deterministic ordered finding list.
- `src/timeline.sai` maintains the independent chronological event view.
- `src/lib.sai` exposes the fixture audit and expected-result tests.

## Concepts and limits

This example uses records, enums, `Option`, fixed-size arrays, module imports,
mutable state, loops, insertion ordering, and test declarations. It is an
in-memory audit over bounded inputs: four evidence records, four actors, and
20 events. A duplicate evidence ID is reported against the later record, but
event lookup then uses the first matching record. Actor IDs use the same
policy: the later duplicate is reported and authorization lookup uses the
first row. These policies make invalid inputs deterministic.

The sample does not parse external evidence logs, compute cryptographic
hashes, validate real-world identity credentials, perform filesystem or
database I/O, interpret calendar timestamps, or enforce jurisdiction-specific
retention rules. A disposition event is treated as terminal even if it has a
finding, allowing later events to be reported as post-disposition activity.
`audit_fixture()` returns typed arrays rather than printing them; the table
above is the intended human-readable rendering.

## Review required

- Treat the supplied event-array order as the source custody stream. Timestamp
  regressions are findings against that order, while the returned timeline is
  sorted by timestamp for review. This keeps an out-of-order source record
  visible rather than silently accepting a reordered history.
- Represent time as minutes after 09:00 on one fictional date. This keeps the
  example focused on custody rules without implying calendar, timezone, or
  daylight-saving support.
- Treat a disposition action as terminal even when its seal state or value is
  invalid. The report can then show later activity as post-disposition instead
  of treating a defective disposition as if it never happened.
- When the evidence register contains a duplicate ID, report the later
  duplicate and resolve event references to the first register row. This is a
  deterministic diagnostic policy for invalid input, not an assertion that
  the first row is authoritative in a real investigation.
- When the actor register contains a duplicate ID, report the later row and
  resolve actor references to the first row. The finding carries that actor
  ID with evidence ID and event ID set to the reserved synthetic value `0`.

`sai fmt --check sample/digital-evidence-custody-auditor` passes with the
current formatter. `sai check --target wasm32-core --tests --offline`, run
from this package directory, reports that Stage 0 `sai check` and `@derive`
are not implemented. Type checking and test execution therefore remain
unverified; this README makes no claim that the package builds or runs.
