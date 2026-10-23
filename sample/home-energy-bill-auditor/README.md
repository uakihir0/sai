# Home Energy Bill Auditor

Home Energy Bill Auditor compares two fictional electricity tariffs for a
month of household usage. A flat plan charges one rate for every imported
kilowatt-hour. A time-of-use plan charges separate peak and off-peak rates,
plus a demand charge based on the month's maximum kilowatts. Both plans can
apply a solar export credit.

For a solar household using 150 peak and 450 off-peak kWh, exporting 50 kWh,
and reaching 3 kW of peak demand, the hand-calculated comparison is:

| Plan | Energy charge | Export credit | Demand charge | Service charge | Tax | Total |
|---|---:|---:|---:|---:|---:|---:|
| FLAT-SOLAR | $90.00 | −$2.00 | $0.00 | $9.00 | $4.85 | $101.85 |
| TOU-SOLAR | $64.50 | −$2.00 | $18.00 | $9.00 | $4.47 | $93.97 |

For a peak-heavy household using 500 peak and 100 off-peak kWh, with 12 kW of
demand and no exports, the flat plan totals $103.95 while the time-of-use
plan totals $193.20. The cheaper plan therefore depends on the shape of the
load, not only its monthly kWh.

## Billing rules

- All prices and totals use integer cents; tax rounds down to a whole cent.
- Monthly energy charges are calculated before solar export credits.
- Export credits are limited to the energy charge. They cannot reduce the
  service charge, demand charge, or tax below zero.
- Tax applies to the energy charge after export credits, plus demand and
  service charges.
- A comparison preserves input plan order in its quote array. Equal totals
  recommend the first plan in that input order.
- Monthly imports above 10,000 kWh, exports above 10,000 kWh, and peak demand
  above 500 kW are rejected before multiplication. Plan rates and fixed
  charges have separate documented bounds so `U32` intermediate arithmetic
  remains within range.

The project is a pure `wasm32-core` library using fixed data and no I/O. It
demonstrates typed tariff models, validation before arithmetic, capped
credits, integer tax rounding, and deterministic plan comparison. Its source
tests state the expected results, but they have not been executed: the current
compiler does not yet complete semantic checking for this package, and the
Stage 3 `sai test` command is not implemented. The figures above are manually
calculated examples, not a utility tariff or billing advice.
