# Backup Restore Readiness Auditor

Backup Restore Readiness Auditor checks a fixed set of full and incremental
snapshots against each service's recovery point objective (RPO) and recovery
time objective (RTO). It follows each incremental snapshot to its full parent,
checks every item in the restore chain, and selects the newest usable recovery
point. A newer corrupt or otherwise unusable snapshot does not hide an older
point that still meets both objectives.

The fixture is evaluated at minute 1000:

| Service | Result | Restore point | Data age | Restore time | Newer point skipped | Catalog issues |
|---|---|---:|---:|---:|---|---:|
| Payments | Ready with fallback | 1002 | 50 min | 25 min | 1, checksum mismatch | 0 |
| Catalog | Not ready | — | — | — | RPO exceeded | 0 |
| Orders | Not ready | — | — | — | RTO exceeded | 0 |
| Archive | Not ready | — | — | — | Required offsite copy missing | 0 |
| Search | Not ready | — | — | — | Parent snapshot missing | 0 |
| Ledger | Not ready | — | — | — | No snapshots | 0 |
| Billing | Not ready | — | — | — | Retention expired | 0 |

RPO and RTO limits are inclusive. For Payments, snapshot 1002 is exactly 50
minutes old and its complete restore chain takes exactly 25 minutes. Snapshot
1003 is newer, but its checksum is marked invalid, so the auditor selects 1002
and records the fallback.

## Audit flow

1. `data.sai` defines seven service policies and ten fictional snapshots.
2. `audit.sai` checks service IDs and snapshot references across the catalog.
   Duplicate service IDs make every matching service not ready. Orphan
   snapshots and duplicate snapshot IDs appear in `AuditReport.catalog_issues`.
3. The auditor walks each single-parent chain toward its full snapshot. It
   detects missing parents or full roots, cycles or chains beyond the fixed
   catalog size, cross-service parents, invalid timestamp order, checksum
   failures, expired retention, missing offsite copies, and restore-time
   overflow.
4. A chain is eligible only when every snapshot passes its checks and the
   candidate meets both the service RPO and RTO.
5. If more than one candidate is eligible, the newest capture time wins. A
   same-time tie goes to the lower numeric snapshot ID. Newer rejected
   candidates are counted and the newest rejection reason is retained.
6. `lib.sai` exposes `audit_fixture()`. Source-level tests cover the selected
   fallback, inclusive objective limits, catalog identity failures, each
   fixture finding, and stable ordering.

Check `catalog_issue_count` before interpreting service findings as an
auditable whole: an orphan snapshot is reported there even when it does not
belong to a known service's restore chain.

## Project structure and concepts

- `model.sai` defines service objectives, snapshot lineage, issues, findings,
  catalog issues, and aggregate counts.
- `data.sai` contains a bounded, fictional backup catalog.
- `audit.sai` validates restore chains and selects a deterministic recovery
  point for each service.
- `lib.sai` exposes the fixture audit as a pure library function.

The project illustrates fixed-capacity data, parent-chain traversal, cycle
detection, `Option` and `Result`, overflow-safe duration accumulation,
service-level policies, deterministic tie-breaking, fallback selection, and
auditable failure reasons.

## Review required

- RPO/RTO values, restore durations, retention deadlines, and the requirement
  to store every item in a chain offsite are teaching assumptions. A service
  owner must define them from its recovery plan and verify them with actual
  recovery exercises.
- If a candidate exceeds both RPO and RTO, the finding reports the RPO breach
  first. Operations teams should confirm that this is the most useful alert
  priority for their recovery process.
- `checksum_valid` is a fixture flag; the example does not read backup bytes or
  perform cryptographic verification. Offsite and retention fields are also
  trusted metadata, not independently verified storage facts.
- All times are whole minutes on one shared clock. The sample does not model
  time zones, clock skew, partial snapshots, encryption-key availability,
  backup application consistency, restore dependencies, or concurrent
  catalog updates.
- It considers each snapshot as a candidate and uses a single-parent chain.
  It does not handle branched, deduplicated, or multi-parent backup formats.
- A “Ready” result means only that the supplied fixture satisfies these
  simplified checks. It does not prove that a service can be recovered.

## Package and compiler status

This is a pure `wasm32-core` library with no I/O, external dependencies, or
persistent state. The package uses fixed service and snapshot capacities.
`sai fmt --check` validates the source syntax and canonical formatting. The
current compiler rejects `sai check` at its unimplemented Stage 0 feature gate
and `sai test` at its unimplemented Stage 3 feature gate. The source-level
tests therefore cannot yet run, and the result table is hand-checked against
the fixture, not compiler-generated output.
