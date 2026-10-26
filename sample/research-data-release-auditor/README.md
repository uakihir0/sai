# Research Data Release Auditor

This fixed-fixture auditor decides whether research datasets are ready for a
proposed audience. It checks source manifests and provenance, source-license
scope, every listed contributor's consent, de-identification evidence and a
scope-dependent disclosure-risk score, schema and content checksum state,
legal holds, retention, embargo dates, project access policy, and derived
dataset dependencies.

It reads a bounded in-memory fixture and produces a deterministic decision for
each dataset. It performs no network, filesystem, database, registry, or
publication operation. A `Released` result is a teaching-fixture decision, not
permission to publish real research data.

## Hand-checked fixture result

The fixture is reviewed on day 400. A retention end day is inclusive; an
embargo end day is the first day on which release is permitted. Dataset
records are listed in dependency order, with each parent before its derived
children.

| Dataset | Scope | Decision | First finding |
|---|---|---|---|
| DS-FIELD-001 | Public | Released | All source, consent, privacy, integrity, retention, and embargo evidence passes. |
| DS-INTERVIEW-001 | Public | Rejected | P-201 consent permits controlled access only. |
| DS-SENSOR-2026 | Controlled | Held | Its source custody chain is marked broken. |
| DS-MOBILITY-003 | Controlled | Released | Its parent DS-FIELD-001 is released; both contributors permit controlled access. |
| DS-PUBLIC-MOBILITY-004 | Public | Held | Its parent DS-INTERVIEW-001 is rejected. |
| DS-CENSUS-LEGACY | Controlled | Rejected | Retention ended on day 399, before the day-400 review. |
| DS-TRIAL-EXTRACT | Controlled | Held | A legal hold pauses release. |
| DS-CLIMATE-EMBARGO | Public | Held | Embargo remains active through day 404. |
| DS-SCHEMA-DRIFT | Controlled | Held | The declared schema check failed. |
| DS-CHECKSUM-FAIL | Public | Held | The content checksum does not match. |
| DS-DISCLOSURE-RISK | Public | Held | Risk score 4 exceeds the public-scope limit of 1. |
| DS-LICENSE-RESTRICTED | Public | Rejected | The source license permits controlled access only. |
| DS-UNKNOWN-SOURCE | Controlled | Rejected | Manifest references a source absent from the source catalog. |
| DS-PARTIAL-CONSENT | Controlled | Held | P-1202 has consent on file, but expected contributor P-1203 has no matching consent record. |
| DS-WITHDRAWN | Public | Rejected | Contributor P-1301 withdrew consent. |
| DS-FIELD-001 (duplicate) | Public | Rejected | Dataset ID already appears earlier in the review queue. |

Hand-calculated totals: **16 reviewed, 2 released, 8 held, 6 rejected**. The
child mobility summary is eligible after its source parent, while the public
summary remains held because its interview parent did not clear release.

## Decision precedence

Each dataset gets only its first finding. Records are considered in fixture
order. Within one dataset the checks run in this order:

1. Duplicate dataset ID.
2. Missing manifest; manifest-to-dataset mismatch; unknown or unapproved
   source.
3. Incomplete accession/provenance evidence; broken custody chain.
4. Requested scope beyond the source license.
5. Contributor consent: each rostered contributor must have a matching
   record; any incomplete record; any withdrawal; then any scope that is too
   narrow.
6. Requested audience beyond the dataset's project-policy ceiling.
7. Parent reference: unknown parent; parent not yet evaluated; parent not
   released.
8. Missing de-identification attestation; disclosure risk above the selected
   audience's limit.
9. Invalid schema; checksum mismatch.
10. Legal hold; expired retention; active embargo.
11. Otherwise, release.

The order makes provenance and authority failures visible before data-quality
findings, then gates derived data on its parent's decision before examining
its own privacy and integrity checks. Consent checks compare each dataset's
expected contributor IDs with rows for that dataset. Missing evidence has
precedence over withdrawal, which has precedence over scope mismatch. Within
a class, the fixture is deterministic and the finding does not identify a
particular consent row.

Unknown sources, invalid source or consent scope, withdrawn consent, unknown
parents, expired retention, and duplicate IDs are `Rejected`. Other findings
are `Held` so evidence can be repaired or the governing condition can change.
A derived record with a parent later in the input is held as `ParentNotReady`;
the supplied fixed fixture itself is topologically ordered.

## Modules

- `src/model.sai` defines scopes, evidence records, dataset proposals, and
  bounded audit results.
- `src/fixtures.sai` provides three source entries, 16 manifests, 17 consent
  records, and 16 dataset proposals.
- `src/lookup.sai` resolves stable IDs and fixture records.
- `src/policy.sai` checks scope ceilings, contributor consent, risk thresholds,
  and reject-versus-hold classification.
- `src/lineage.sai` requires a derived dataset's parent to have been evaluated
  and released.
- `src/audit.sai` applies the first-finding order and calculates counts.
- `src/lib.sai` exposes `sample_audit()` and contains source-level assertions
  for the decisions, contributor coverage, lineage gate, and scope handling.

All evaluation is pure and deterministic over fixed-capacity arrays. The
review day is part of the fixture input, so rerunning the same input cannot
change a decision based on the wall clock.

## Limits

This is not a data-protection, research-ethics, repository, or publication
service. It does not inspect dataset bytes, recompute checksums, validate
schema files, measure re-identification risk, authenticate consent, verify
source licenses, contact contributors, process deletion requests, or enforce
legal holds. Evidence is represented by fixture booleans and scalar values.
Scope is a three-level teaching hierarchy; jurisdictions and consent
instruments can have purpose, geography, duration, recipient, and onward-use
conditions that cannot be captured here.

Each dataset has one source manifest, at most one direct parent, and at most
two expected contributor IDs. Input records and roster counts are expected to
fit their fixed capacities, dataset IDs are treated as exact strings, and
review order is the input-array order. `ParentNotReady`
fails closed for a forward reference instead of topologically reordering
arbitrary input. This model does not detect cycles as a separate graph
diagnostic, traverse multiple ancestors, or calculate whether derived outputs
retain privacy risk inherited from a parent. Release decisions do not reserve
storage, alter retention, remove data, or publish an artifact.

## Review required

- The sample uses `Internal < Controlled < Public` as a broadness ordering.
  Public and controlled findings are checked against the narrowest source,
  contributor, and project-policy ceiling. Production consent may govern
  purpose, recipient type, geography, and onward sharing independently, so a
  single ordering can grant too much or unnecessarily block a release.
- Each dataset lists its expected contributors in a fixed two-slot roster, and
  every listed person needs a matching consent row. A missing row for even one
  person makes the proposal held, even if another contributor has a valid row.
  Missing evidence precedes withdrawal or narrow scope; the sample rejects
  withdrawal while missing consent is held. A real policy should define
  whether withdrawal applies retroactively to derived outputs and how
  duplicate or superseded consent records are reconciled.
- De-identification is a required attestation at every release scope. The
  simple score limits are 1 for public, 3 for controlled, and 6 for internal.
  These are illustrative, not scientific thresholds; a real disclosure review
  needs a documented threat model, access context, and qualified reviewer.
- Legal hold takes precedence over retention expiry, and the hold is modeled
  as a release pause. Actual preservation and disclosure duties may differ,
  and a hold may prohibit deletion without authorizing release.
- Retention end days are inclusive and expiry rejects a proposal. Embargo end
  days are the first release-eligible day and an active embargo is held.
  Production policy must select the controlling event, timezone, extensions,
  and whether an expired record should be deleted, archived, or escalated.
- A source manifest is represented by booleans and one source ID, while the
  content checksum is represented as a verified flag. Neither is cryptographic
  proof. Production review must bind immutable manifest bytes, source
  provenance, dataset version, and checksum algorithm.
- Parent decisions gate derived data, but this fixture accepts one parent and
  relies on records being supplied in dependency order. A production lineage
  service should detect cycles, reconcile multiple parents, propagate
  revocations, and re-evaluate descendants when upstream evidence changes.
- Rejected means this proposal cannot proceed under its current identity or
  authority evidence; held means a condition can potentially change. This
  split is a demonstration policy and should be approved by the institution's
  data access and research governance owners.

## Formatting and compiler status

Run `sai fmt --check sample/research-data-release-auditor` to check formatting.
The result table is hand-calculated, not compiler output. Embedded assertions
document intended behavior. The current compiler's Stage 0 `sai check` and
`@derive` gates prevent this project from being type-checked or executed, so
the sample does not claim runtime verification.
