# Smart Building Comfort Controller

This sample plans bounded thermostat setpoints and ventilation commands for a
small building. It evaluates four fixed time slices for a nursery, studio, and
archive. Each result keeps the requested setpoint, comfort-bounded target,
commanded setpoint, ventilation minimum, delivered ventilation, and explicit
unmet amounts in an in-memory audit record.

## Fixture report

Temperatures and setpoints are stored in tenths of a degree Celsius. The
hand-checked fixture produces the following period movement limits and
commands:

| Period | Price / carbon signal | Allowed movement | Used | Notable result |
|---|---|---:|---:|---|
| 06:00 | Low / low | 30 | 15 | Nursery excursion moves 22.5°C to 24.0°C; its 25.0°C request is clamped to its 24.0°C comfort limit. |
| 12:00 | High / high | 10 | 10 | Archive excursion is served before the studio; the studio request remains unmet while the signal cap is used. |
| 15:00 | Low / high | 20 | 15 | Stable studio request moves the command to 23.0°C, stopping at the per-period equipment limit. |
| 18:00 | Elevated / low | 25 | 0 | A one-period change in the studio request waits for a second matching sample. |

The archive's 12:00 CO₂ reading calls for eight ventilation units, while its
fan limit supplies four. The nursery's 06:00 minimum is eight units and its fan
supplies six. Those two shortfalls remain visible in the per-zone audit and in
the aggregate summary. Across the fixture, the controller uses 30 tenths of a
degree of heating-direction movement and 10 of cooling-direction movement, for
40 total. The report also counts nine zone-periods with a raw setpoint request
not fully met, two ventilation shortfalls, and six unmet ventilation units.

## Control rules

- A measured temperature outside its comfort band moves ahead of in-band
  requests. Remaining requests are ordered by lower numeric priority, then a
  stable zone order. This reading affects urgency only; movement direction and
  amount come from the comfort-bounded requested target relative to the prior
  commanded setpoint.
- Requested setpoints are clamped to each zone's comfort band. A comfort
  excursion bypasses target confirmation; otherwise, a changed target must be
  observed in two consecutive samples before it can move the command.
- Heating and cooling have separate movement capacities. Each zone also has a
  maximum setpoint change per period. Remaining capacity is assigned in
  service order, and partial movement is recorded as unmet request.
- Price and carbon signals each reduce the total movement allowance. The
  ventilation minimum is computed separately from occupancy and CO₂, then
  enforced up to each zone's fan limit. A fan shortfall is reported instead of
  being hidden by thermal or demand-response decisions.
- `report.sai` aggregates the same structured audit records without external
  I/O. `lib.sai` exposes the fixed run and includes regression assertions.

## Modules

- `model.sai` defines the readings, equipment limits, persistent controller
  memory, per-zone audit records, and aggregate report.
- `data.sai` supplies equipment limits, the single canonical starting-memory
  fixture, and four fixed snapshots.
- `policy.sai` defines comfort bounds, ventilation minimums, signal penalties,
  and deterministic service ordering.
- `controller.sai` applies target confirmation, capacity limits, and ventilation
  safety accounting to one snapshot.
- `report.sai` summarizes a run, and `lib.sai` runs the fixture and checks its
  expected decisions.

## Review required

- The price and carbon levels are ordinal fixture inputs. Their penalties of
  five or ten movement units are an illustrative dispatch policy, not an
  electricity tariff conversion, emissions estimate, or optimal schedule.
- A movement unit represents one tenth of a degree of commanded setpoint
  movement for accounting only. It is not a unit of delivered heat, cooling,
  electricity, or thermal response. The sample does not simulate building
  dynamics or predict future room temperature.
- The comfort bands, CO₂ thresholds, ventilation minima, equipment capacities,
  and two-sample confirmation rule are fixed demonstration assumptions. They
  are not a building code, clinical standard, calibrated sensor policy, or
  safety certification.
- Measured temperature is used only to prioritize out-of-band readings and
  bypass target confirmation. The bounded requested target determines the
  movement direction relative to the previous command. A hot room can therefore
  receive a heating-direction command when its requested target is above the
  previous command. This is a command-allocation demonstration, not
  temperature-feedback control.
- Ventilation is treated as an independent command. The fixture exposes fan
  capacity shortfalls, but it does not control dampers, prove air exchange,
  or model outdoor air quality, humidity, pressure, or interlocks.
- Input validity is assumed: exactly one reading for each zone per snapshot,
  valid comfort bounds, nonnegative capacities, and correctly ordered samples.
  Sensor faults, missing data, stale signals, actuator feedback, persistence,
  scheduling, and network or hardware interfaces are out of scope.

## Package and compiler status

This is a pure `wasm32-core` package with fixed in-memory fixtures and no
runtime I/O or external dependencies. The policy and expected totals are
hand-checked. `sai fmt --check sample/smart-building-comfort-controller` passes for the whole
package. The current `sai check --tests --offline` command stops at Stage 0 and
reports `sai check` and `@derive` as unimplemented, so the SAI assertions have
not executed. The expected decisions and totals above are hand-calculated;
formatting is not a semantic or runtime guarantee.
