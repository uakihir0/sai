# Battery Recycling Chain Auditor

Battery Recycling Chain Auditor reviews a fixed queue of outbound battery
black-mass manifests. It follows each declared source lot through custody
evidence, shipment-permit scope and dates, destination authorization, assay,
and a feed-to-output mass balance. Only cleared manifests consume source-lot
stock or permit quota.

## Hand-checked fixture result

The fixture has nine proposed manifests. The expected result below is
calculated from the fixed inputs; it is not output produced by the current
compiler.

```text
manifests reviewed: 9
cleared: 2
held: 5
rejected: 2
credited feed: 900 kg
credited recovered metal: 180 kg

source lot NMC-44: 900 / 1,400 kg credited; 500 kg remain
source lot LFP-12: 0 / 800 kg credited; 800 kg remain
permit PER-NMC-240: 900 / 1,200 kg credited; 300 kg remain
permit PER-LFP-240: 0 / 1,000 kg credited; 1,000 kg remain
```

| Manifest | Feed calculation | Result | First finding |
|---|---:|---|---|
| RC-401 | 620 gross - 20 tare = 600 kg | Cleared | 20% assay predicts 120 kg; 120 + 450 residue + 30 loss = 600. |
| RC-402 | 310 gross - 10 tare = 300 kg | Cleared | 20% assay predicts 60 kg; 60 + 225 residue + 15 loss = 300. |
| RC-403 | 360 gross - 10 tare = 350 kg | Held | The NMC permit has 300 kg left after RC-401 and RC-402. |
| RC-404 | 160 gross - 10 tare = 150 kg | Rejected | Shipment day 241 is after PER-LFP-240's inclusive day-240 expiry. |
| RC-405 | 155 gross - 5 tare = 150 kg | Held | Required custody documents are incomplete. |
| RC-406 | 210 gross - 10 tare = 200 kg | Held | 30% assay predicts 60 kg, but 90 kg is reported; 4 kg is the allowed variance. |
| RC-407 | 105 gross - 5 tare = 100 kg | Held | 30 + 60 + 9 = 99 kg, leaving 1 kg of feed unreconciled. |
| RC-408 | 100 gross - 5 tare = 95 kg | Rejected | BULK-DEPOT is unlicensed and does not accept LFP black mass. |
| RC-409 | 560 gross - 10 tare = 550 kg | Held | NMC-44 has only 500 kg remaining after earlier clearances. |

An assay's allowed recovered-metal variance is the greater of 1 kg or 2% of
feed mass, with integer division rounded down. The mass balance itself must
close exactly: recovered metal + residue + process loss must equal feed mass.
For the two cleared NMC shipments, permit use is 600 + 300 = 900 kg and
recovered metal is 120 + 60 = 180 kg. Held and rejected records reserve no
feed and receive no recovery credit.

## Review order and decisions

Each manifest is checked in fixture order and reports its first finding. The
precedence is: repeated manifest ID; unknown or unlicensed source; missing
custody evidence, broken custody chain, or broken seal; unknown permit; permit
material or destination mismatch; permit not yet valid or expired; unknown or
ineligible destination; invalid shipment weight; source-lot quantity; permit
quota; missing or invalid assay; assay variance; and mass-balance closure.

Authority and scope failures are `Rejected`: a record cannot be shipped under
the cited source, permit, date, or destination. Evidence, quantity, and
measurement failures are `Held`: a reviewer could correct or supplement the
record before release. A manifest ID is considered used even if its earlier
proposal was held or rejected. Only cleared manifests reserve stock and
permit capacity, and later proposals see those earlier clearances.

## Project structure

- `model.sai` defines the fixed audit input, authority records, manifests,
  findings, decisions, and balances.
- `fixtures.sai` supplies two source lots, three permits, two destinations,
  and nine proposed manifests.
- `lookup.sai` resolves references, totals earlier clearances, applies the
  destination test, and maps findings to hold or reject outcomes.
- `auditor.sai` checks each manifest and produces deterministic decisions,
  source balances, permit balances, and credited totals.
- `lib.sai` exposes `sample_audit()` and states assertions for the expected
  fixture results.

The sample uses kilograms and whole-number assay percentages. Arithmetic is
bounded for these fixtures and is not a general overflow-safe accounting
implementation. Input array counts are assumed to fit their declared
capacities; source-lot capacity is supplied as an available mass, while permit
quota is accounted by cleared feed mass.

## Limits

This is a fixed-fixture review, not a transport, recycling, or regulatory
system. It does not authenticate documents, query permit registries, validate
jurisdictional rules, measure a physical seal, select carriers, schedule
shipments, or model multiple custody transfers as individually signed events.
Each accepted manifest is assumed to move one material stream from one
registered lot to one destination. Mixed-lot loads, split loads, returns,
rework, multi-step processing, and storage inventories require additional
lineage records and reconciliation rules.

## Review required

- The rule classifies authority and scope defects as rejects while treating
  incomplete evidence, quota, assay, and mass-balance issues as holds. A real
  operator may need different escalation or correction paths, especially for
  an expired permit discovered after a shipment has already moved.
- Proposals are processed in fixture order. Earlier cleared manifests claim
  permit quota first; this is deterministic and easy to audit, but it does not
  represent dispatch priority or fairness.
- The permit's `valid_through_day` is inclusive, and dates are modeled as
  integer day numbers. Production policy must specify time zones, shipment
  cutoffs, amendments, and whether authorization is checked at dispatch,
  arrival, or both.
- The assay variance is fixed at max(1 kg, 2% of feed), and mass closure has
  zero tolerance. These thresholds are teaching choices; material,
  instrument, sampling, and jurisdiction-specific requirements may need
  separate tolerances and independent review.
- A Boolean license and destination-acceptance flag stand in for live,
  scoped credentials. The sample assumes their fixture values are correct and
  current; no external registry or certificate is verified.
- A single assay percentage predicts recovered metal by integer
  multiplication and truncation. The model omits element-specific assays,
  moisture, sampling uncertainty, hazardous residues, and measurement
  uncertainty. Its expected values are not operational or environmental
  guidance.

## Compiler status

The package manifest targets `wasm32-core`. The `sai fmt --check` result is
passing for this package. `sai -C sample/battery-recycling-chain-auditor check
--offline --target wasm32-core` stops at the current Stage 0 `sai check` and
`@derive` feature gates. It reports no source syntax errors, but type checking
and the embedded source assertions have not been executed.
