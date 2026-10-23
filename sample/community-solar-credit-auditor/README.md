# Community Solar Credit Auditor

Community Solar Credit Auditor reconciles one month's shared solar generation
against a community's subscriber contracts and utility credit statements. It
validates each household's meter and allocation-cap data, allocates a fixed
generation pool in contract-priority order, then reports missing evidence,
credits posted to ineligible accounts, and over- or under-allocation.

The June 2026 fixture has 400 kWh of generation. A subscriber's eligible
demand is the smaller of valid metered usage and their valid monthly contract
cap. Lower numeric priority is served first; account ID breaks ties. The
allocator processes the entire demand at each rank before moving on and stops
when the generation pool is empty.

## Hand-checked June result

| Rank | Account | Usage / cap (kWh) | Eligible demand (kWh) | Planned (kWh) | Reported (kWh) | Findings |
|---:|---|---:|---:|---:|---:|---|
| 1 | 101 Aki | 200 / 150 | 150 | 150 | 155 | Over-allocated by 5 |
| 2 | 105 Emi | missing / 80 | 0 | 0 | 12 | Missing meter usage; 12 kWh credited to an invalid account |
| 3 | 106 Fumi | 130 / 300 | 0 | 0 | 0 | Contract cap exceeds the 250 kWh limit |
| 4 | 102 Ben | 110 / 100 | 100 | 100 | 90 | Under-allocated by 10 |
| 5 | 107 Goro | 75 / missing | 0 | 0 | 0 | Missing contract cap |
| 6 | 103 Chika | 250 / 200 | 200 | 150 | 155 | Over-allocated by 5 |
| 7 | 108 Hana | 6001 / 70 | 0 | 0 | 0 | Meter usage exceeds the 6000 kWh limit |
| 8 | 104 Dai | 90 / 75 | 75 | 0 | missing | Missing credit statement |

Four valid households request 525 kWh in total. The 400 kWh pool fully serves
Aki and Ben, then gives the remaining 150 kWh to Chika; Dai receives zero
because the pool is exhausted. The planned allocation totals 400 kWh, leaving
none unused.

Seven supplied statements report 412 kWh: `155 + 12 + 0 + 90 + 0 + 155 + 0`.
That is 12 kWh above the generation pool. The account-level reasons explain
the difference: Aki and Chika are 5 kWh over plan each, Ben is 10 kWh under
plan, and Emi has 12 kWh posted despite missing meter data. Dai's statement
is absent, so 412 kWh is the recorded statement total and 12 kWh is a lower
bound on the source overage; the sample does not assume the missing statement
was zero.

The summary therefore contains 8 reviewed households, 4 valid households,
525 kWh of eligible demand, 400 kWh planned, 7 statements totaling 412 kWh,
2 valid-account over-allocation findings totaling 10 kWh, 1 valid-account
under-allocation totaling 10 kWh, 1 invalid-account credit totaling 12 kWh,
1 missing meter, 1 out-of-range meter, 1 missing contract cap, 1 out-of-range
contract cap, and 1 missing statement.

## Audit rules

- Missing meter usage, meter usage above 6000 kWh, missing contract cap, and a
  zero or greater-than-250 kWh contract cap each make a subscriber ineligible.
- If more than one data defect is present, the first applicable reason in
  that order is reported. Data findings are separate from credit-statement
  findings, so one household can appear in both groups.
- For valid subscribers, planned credits are capped by both their eligible
  demand and the generation remaining at their rank. The deterministic rank is
  priority ascending, then account ID ascending.
- A positive statement for an ineligible subscriber is reported as a credit
  to an invalid account. Statements for eligible subscribers are compared
  with planned credits to identify over- and under-allocation.
- Missing statements are counted as missing evidence, not as a numeric zero.
  Present statements, including statements for invalid accounts, remain in
  the reported total so that the source-level reconciliation does not hide
  posted credits.

## Source map and limits

`model.sai` defines subscriber records, reason codes, and account/monthly audit
reports. `fixtures.sai` contains the fixed generation, validation limits, and
deliberately unsorted eight-household input. `validation.sai` classifies data
quality and calculates eligible demand. `ranking.sai` provides deterministic
priority order, and `allocation.sai` builds the generation-constrained plan.
`audit.sai` compares statements to the plan and accumulates reason-specific
totals. `lib.sai` exposes the June audit and source-level expected-result
assertions.

This is an illustrative, in-memory `wasm32-core` package. It does not model
real utility rules, monetary settlement, rollover credits, household
enrollment changes, partial-period service, duplicate account detection,
arbitrary external data, or overflow for unbounded inputs. Its fixtures are
fixed-size by design. Assertions in `lib.sai` document expected behavior;
they are not claimed as executed tests. The current reference compiler's
`sai fmt --check sample/community-solar-credit-auditor` passes. Semantic
checking is blocked: `sai check --target wasm32-core --tests --offline`
reports that Stage 0 `sai check` and the `@derive` attribute are not
implemented, so type checking and test execution have not been verified.
