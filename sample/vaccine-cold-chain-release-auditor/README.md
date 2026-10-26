# Vaccine Cold-Chain Release Auditor

This fixed-fixture project reviews four fictional vaccine shipments before a
release decision. It keeps observed shipment and logger facts separate from
the teaching policy in `src/policy.sai`. Each decision retains every finding:
duplicate shipment IDs, unknown or expired lots, invalid quantities, and
critical excursions reject a shipment; any other finding holds it, and only a
finding-free shipment is released.

**This is not clinical guidance or a validated cold-chain system.** Every
temperature threshold and decision below is a fictional software-modeling
choice. Nothing here establishes product stability, safe handling, or
authorization to use a real vaccine.

## Hand-checked fixture

The review day is day 400. Expiration and calibration validity are inclusive
through their recorded day.

| Shipment | Dose request | Logger facts | Custody facts | Findings | Decision |
|---|---:|---|---|---|---|
| CC-701 | 700 | Full 60-minute coverage; no out-of-band interval; current calibration | All five checks pass | None | Released |
| CC-702 | 600 | Full coverage; one 10-minute interval ranges from 9–10°C; current calibration | All five checks pass | Insufficient usable stock: only 500 doses remain after CC-701 | Held |
| CC-703 | 300 | Calibration expired on day 390; 15-minute logger gap; 30 out-of-band minutes | Carrier handoff missing, chain broken, seal compromised | Calibration out of date; logger gap; excursion review required; missing carrier handoff; custody chain broken; seal compromised | Held |
| CC-704 | 50 | Current calibration; 70 out-of-band minutes | Delivery evidence missing | Lot expired; critical excursion; missing delivery evidence | Rejected |

The policy accepts up to 20 cumulative out-of-band minutes for release. More
than 20 and up to 60 requires review and holds the shipment; more than 60 is a
critical rejection. A temperature segment contributes its *entire duration*
when its recorded range crosses outside 2–8°C. CC-702's 10-minute excursion is
within the model's release allowance, so it does not add a finding.

The outcomes are one Released, two Held, and one Rejected shipment. Only
CC-701 reserves inventory: it releases 700 of LOT-A's 1,200 doses, leaving
500 usable doses. LOT-B retains all 500 doses because CC-703 is held.
LOT-C's 100 doses are expired by day 400, so it has zero remaining usable
doses.

## Modules

- `src/model.sai` declares the bounded input records, telemetry assessment,
  findings, decisions, and stock balances.
- `src/fixtures.sai` supplies three lots, three logger calibration records,
  and four shipment proposals with explicit interval and custody evidence.
- `src/policy.sai` contains the temperature band, logger-gap limit,
  excursion thresholds, and reject-versus-hold rule.
- `src/logger.sai` checks interval ordering and coverage, totals uncovered
  time, and conservatively totals out-of-band duration.
- `src/audit.sai` checks lots, logger/calibration facts, custody evidence,
  excursion results, and available stock; then aggregates outcomes and
  remaining usable doses.
- `src/lib.sai` exposes `audit_fixture()` and embeds assertions for the
  hand-checked results and boundary behavior.

## Review required

- The 2–8°C band, 10-minute maximum uncovered gap, 20-minute release allowance,
  and 60-minute critical threshold are invented teaching values. They must
  not be interpreted as clinical, manufacturer, transport, or regulatory
  limits.
- Rejection is assigned to duplicate shipment IDs, unknown lots, expired lots,
  invalid quantities, and critical excursions; other findings hold. This
  priority is an illustrative audit policy, chosen to make duplicate identity,
  lot eligibility, quantity validity, and severe excursion findings stop a
  release while preserving less severe evidence gaps for review. Every record
  in a duplicate shipment-ID set is rejected before any of those records can
  reserve stock. It is not a disposition rule for actual products.
- Expiration and calibration end days are inclusive, and all dates use
  integer day numbers. Real use would need an authoritative clock, timezone,
  product-specific labeling, and explicit rules for the point in the journey
  at which eligibility is evaluated.
- Each interval's low/high values summarize its entire span. If any part of
  that range leaves the allowed band, this model counts the full interval as
  excursion time. Missing spans are unknown, not presumed to have remained
  in range. Calibration and logger facts are still modeled as supplied facts;
  the sample does not validate their source.
- Shipments are processed in fixture order, so earlier released shipments
  claim stock first. This is deterministic but does not represent a real
  dispatch priority or allocation policy.
- Custody booleans stand in for pickup, carrier handoff, delivery, chain, and
  seal evidence. They do not verify signatures, identities, physical seals,
  or document authenticity.
- The example assumes input counts fit each declared array and the bounded
  minute and dose totals fit `U32`; every slot below a count must be `Some`,
  and unused trailing slots are ignored. Missing entries inside a counted
  prefix are outside this fixture model and would need explicit malformed-row
  findings in a production implementation. It does not ingest files, contact
  logger services, evaluate vaccine stability, or authorize release of real
  stock.
- Lot and logger IDs are expected to be unique. Lookups currently use the
  first matching fixture record, so duplicate catalog identifiers need an
  explicit production policy and diagnostic.

## Compiler status

The package targets `wasm32-core`. The repository's current Stage 0 compiler
passes `sai fmt --check` for this directory. `sai check --target wasm32-core
--tests --offline` reaches the package parser, then reports that `sai check`
and `@derive` are not implemented in Stage 0; type checking and embedded
assertion execution are therefore unavailable. The sample makes no runtime
or clinical validation claim.
