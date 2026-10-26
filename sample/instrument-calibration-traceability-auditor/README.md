# Instrument Calibration Traceability Auditor

This fixed-fixture SAI library audits a small metrology register. For each
instrument it selects the latest calibration that is not dated in the future,
checks the calibration certificate, range, due date, and result, then follows
the reference-standard chain. It reports a deterministic `Ready`, `DueSoon`,
or `Hold` finding with the first issue in a fixed check order.

All names, ranges, clock values, uncertainty values, and decisions are
fictional teaching data. This example is not a calibration procedure and does
not establish legal or laboratory accreditation.

## Fixture report

The evaluation clock is minute 1400. The due-soon window is 120 minutes,
inclusive. Every range is expressed in fictional milli-units and every
uncertainty in fictional milli-units; no physical unit conversion is implied.

| Instrument | Calibration | Status | Finding |
|---|---:|---|---|
| Optical comparator | 2001 | Ready | Combined uncertainty 3; due in 150 minutes |
| Hydraulic pressure cell | 2002 | Hold | Calibration interval ended at minute 1100 |
| Bench balance | 2003 | Ready | Reference chain uncertainty 2; total uncertainty 3 |
| Thermal chamber | 2004 | Hold | Combined uncertainty 8 exceeds its limit of 6 |
| Flow meter | 2005 | Hold | A reference in its traceability chain was revoked at minute 1350 |
| Reference laser | 2006 | DueSoon | Exactly 120 minutes remain in its calibration interval |

The resulting counts are two `Ready`, one `DueSoon`, and three `Hold`. The
status is a review queue signal, not a release decision. A hold never mutates
the equipment register or disables an instrument.

## Audit flow

1. `data.sai` supplies six instruments, five reference standards, six
   calibration records, and the evaluation clock.
2. `registry.sai` detects ambiguous instrument and standard identities and
   chooses a calibration by newest performed minute, breaking ties with the
   smaller record ID. Future-dated records are not candidates.
3. `audit.sai` checks instrument identity and ordered range, then requires a
   current-dated calibration. It reports a duplicate selected calibration ID
   before the result check, followed by certificate expiry, calibration range,
   due-time overflow, and overdue status.
4. `traceability.sai` walks parent-standard links with a five-entry visited
   set. Every node must be unique, accredited, unrevoked at evaluation time,
   valid when the calibration occurred, and wide enough for the full
   instrument range. A missing parent, duplicate identity, cycle, or chain
   longer than the bounded register fails closed.
5. The calibration's measured uncertainty and the uncertainty values of every
   standard in the chain are added with overflow checks. The conservative sum
   must not exceed the instrument's configured maximum. The sample then marks
   otherwise-valid certificates `DueSoon` when the remaining interval is at
   most 120 minutes.

## Project structure

- `model.sai` defines instruments, standards, calibration records, findings,
  issues, and aggregate report counts.
- `data.sai` contains the fixed fictional register.
- `registry.sai` implements identity resolution and stable calibration
  selection.
- `traceability.sai` validates a bounded parent chain and accumulates its
  uncertainty bound.
- `audit.sai` applies gates in order, reports catalog inconsistencies, and
  includes regression cases for fallback prevention, ordered range coverage,
  cycle detection, revocation, uncertainty, and due-date boundaries.
- `lib.sai` exposes `fixture_audit()` for inspection.

## Package and compiler status

This is a dependency-free `wasm32-core` library with fixed-size arrays and no
file, network, hardware, or persistent-state access. Run
`sai fmt --check sample/instrument-calibration-traceability-auditor` from the
repository root to check source parsing and canonical formatting. The fixture
report is hand-derived from the documented rules; it is not compiler output.

The embedded SAI tests express intended results. In the current repository,
`@derive` and the Stage 0 checker are not fully implemented, and the Stage 3
test runner is also unavailable, so the sample has not been type-checked or
executed by SAI.

## Review required

- **Malformed instrument ranges:** A minimum greater than the maximum is held
  before calibration selection because an inverted interval cannot represent
  valid coverage. The register owner should confirm how malformed source data
  is triaged and corrected.
- **Uncertainty combination:** The fixture adds nonnegative uncertainty bounds
  arithmetically as a conservative teaching rule. A real metrology program
  must define whether inputs are compatible expanded uncertainties, standard
  uncertainties, or hard error bounds and choose a justified propagation
  method, coverage factor, correlation model, and unit system.
- **Revocation timing:** A reference revoked by the evaluation minute
  invalidates any calibration that depends on it, even when the calibration
  predates revocation. Real quality systems need a controlled policy for
  retrospective impact analysis and the effective time of a recall.
- **Validity boundaries:** Standard validity and certificate expiry are
  inclusive through their recorded minute; an instrument is overdue only
  after its calculated due minute. The boundary convention is explicit for
  repeatability and requires confirmation against the governing quality
  system.
- **Latest-record policy:** A newer failed or otherwise invalid calibration
  prevents fallback to an older passing record. This avoids silently choosing
  stale evidence, but organizations may require an investigation or a
  different supersession policy.
- **Range model:** Every node in the traceability chain must cover the full
  instrument range, and both the calibration record and each standard use one
  scalar interval. Real procedures may use multiple ranges, nonlinear
  corrections, resolution, environmental limits, and point-by-point results.
- **Accreditation:** A Boolean `accredited` flag stands in for a verified
  certificate, scope, provider identity, and validity period. The fixture does
  not authenticate any certificate or laboratory.
- **Clock and due interval:** All times are unsigned minutes on one fictional
  clock, with no timezone, source, drift, uncertainty, usage-based interval,
  or maintenance downtime model. The 120-minute notice window and instrument
  intervals are examples that need domain approval.
- **Finding order:** Checks report the first issue in the documented sequence.
  Catalog issues are also returned separately; issue ordering does not claim
  to enumerate every possible defect on each instrument.
- **Operational use:** `Ready` only means the fixture metadata passes these
  rules. It is not a calibration certificate, authorization to use equipment,
  or evidence that a real measurement is traceable.
