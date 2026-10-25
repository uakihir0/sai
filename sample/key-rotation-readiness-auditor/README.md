# Key Rotation Readiness Auditor

Key Rotation Readiness Auditor reviews a small, fictional inventory and three
planned service-key rotations. It checks key lifetimes, old/new key state,
independent escrow custody, workload migration attestations, overlap and grace
windows, and workload dependency order before a plan's scheduled revocation.

The fixture is evaluated at minute 1035:

| Plan | Decision | Migrated / affected | Overlap | Revoke at | Primary issue |
|---|---|---:|---:|---:|---|
| Payments key rotation | Ready | 2 / 2 | 40 min | 1070 | — |
| Data platform key rotation | Plan invalid | 0 / 2 | 40 min | 1010 | Evidence stale |
| Log pipeline key rotation | Plan invalid | 0 / 1 | 40 min | 1070 | Custodian not independent |

The aggregate result is one ready plan, no waiting plans, and two invalid
plans. The catalog has no identity or reference issues. The data plan has two
workload blockers: its recorded cache evidence is stale, and its export
workload has no migration evidence after the grace deadline. Its planned
revocation time has also passed while the old key is still marked active. Its
primary issue is stale evidence because workloads are checked in fixture
order, followed by the missing evidence and overdue revocation. The log plan's
old and new keys name the same escrow custodian team.

## Audit flow

1. `data.sai` defines six key records, six custody records, five workloads,
   four evidence records, two ordering constraints, three plans, and one
   bounded policy.
2. `audit.sai` checks catalog identity and references, then evaluates each
   plan in fixture order.
3. A plan must resolve one active old key and one available new key. The old
   key must remain valid through planned revocation. The new key must be valid
   at cutover and remain valid through the later of evaluation time and
   planned revocation. The overlap, grace, and revoke times must be ordered.
4. Both keys need one custody record with verified escrow and a tested
   recovery procedure. Custodians must differ from the key owner teams and
   from each other.
5. Every workload using the old key must have one attested observation of the
   planned new key. Evidence must fall inside the overlap window and remain
   within the configured age limit. Dependency ordering applies only when
   both endpoint workloads match the plan's old source key and new target key;
   the prerequisite must then be observed strictly before the dependent.
   Workload completeness still selects every workload using the old source
   key, so a different target is reported as a workload issue.
6. Plan identity, key metadata and lifetime, window, custody, and having at
   least one affected workload are fail-fast gates. A failure returns the
   first gate issue with `issue_count` equal to 1. Only plans that pass those
   gates are checked for all workload blockers, then dependency ordering, then
   overdue revocation. The first issue follows that stable order. Outstanding
   work before the grace deadline is `Waiting`; outstanding work after it makes
   the plan `PlanInvalid`. An active old key past its planned revoke minute adds
   a separate overdue issue. Each violating dependency edge contributes one
   issue to the count.

The `CatalogSummary` preserves the first catalog issue and total count. It
checks duplicate IDs and unresolved key or workload references across the
bounded inputs, including key references in migration evidence. Resolve
catalog issues before relying on plan findings that depend on those records.

## Project structure and concepts

- `model.sai` defines keys, custodians, workloads, evidence, dependency edges,
  plans, policy, catalog issues, and findings.
- `data.sai` supplies the fixed fictional inventory and rotation fixtures.
- `lookup.sai` performs bounded identity lookups and detects ambiguous records.
- `audit.sai` checks inventory integrity, plan windows, custody, evidence
  freshness, and strict dependency ordering.
- `lib.sai` exposes `audit_fixture()` and source-level regression cases.

The package demonstrates fixed-capacity collections, deterministic catalog
lookup, multi-stage validation, temporal evidence windows, fail-closed
identity handling, and stable issue selection. It performs no file, network,
or persistent-state operations.

## Review required

- Minute values use one fictional clock with no timezone, clock-skew, or
  timestamp-source model. A real rotation policy must define trusted time
  sources and acceptable skew.
- The new key's expiration is treated as inclusive and must cover the later of
  evaluation time and the old key's planned revocation. This keeps the target
  key valid while evidence is evaluated and through the handoff window; a real
  policy should confirm whether endpoint inclusivity and this full horizon fit
  its key lifecycle.
- The 30-minute minimum overlap and 30-minute evidence freshness limit are
  teaching values. A service owner must choose windows based on measured
  rollout and rollback behavior.
- Evidence is considered current only when its observed key, time, and
  attestation flag match the fixture policy. The auditor does not authenticate
  the attestor or verify telemetry, deployment state, or key use.
- Custody independence means different numeric team IDs, with a separate
  custodian team from each key owner. Real organizations need an explicit
  separation-of-duties policy and independently verified escrow and recovery
  procedures.
- Dependency edges constrain order only when both endpoint workloads match
  the same plan's old source key and new target key. An endpoint using the old
  source key with a different target is still included in workload
  completeness checks, but that edge does not add a dependency-order issue.
  An edge is ignored if either endpoint's source and target keys do not match
  the plan's old-to-new key pair. This example does not derive dependencies
  from a service graph or prove that the listed edges are complete; a real
  cross-plan timeline would need explicit plan references and a shared ordering
  model.
- Validation intentionally fails fast on plan identity, key metadata and
  lifetime, window, custody, and missing-affected-workload gates. For those
  early failures, `issue_count` records only the primary gate issue. Once a
  plan passes the gates, the count includes all workload blockers, each
  violating dependency edge, and overdue revocation. Do not interpret the
  count as a complete inventory of every defect on an early-rejected plan.
- `Ready` means the supplied metadata satisfies these rules at the evaluation
  minute. It neither revokes a key nor proves that a rotation is safe in
  production. The sample performs no cryptographic operation and does not
  inspect or handle secret key material.
- When several issues exist, the finding exposes the first issue and a total
  issue count rather than a complete per-workload issue list. The order is
  deterministic to make fixture comparisons reproducible.

## Package and compiler status

This is a dependency-free `wasm32-core` library over fixed-size in-memory
fixtures. Run `sai fmt --check sample/key-rotation-readiness-auditor` from the
repository root to parse and check formatting. That check passes in this
checkout.

The source-level tests describe intended outcomes but could not run:
`sai check --target wasm32-core --tests --offline` stops at the current
unimplemented Stage 0 checker and `@derive` gates, and `sai test --target
wasm32-core --offline` stops at the unimplemented Stage 3 test runner. The
table above is hand-checked against the fixture and is not compiler-generated
output.
