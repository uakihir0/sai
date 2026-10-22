# Marsh Survey Review

Marsh Survey Review classifies five fixed observation records from two
fictional coastal transects. It checks each record in a documented order,
keeps unverified and protected observations out of accepted totals, and
returns counts by review status and transect.

```text
Marsh survey — Day 14
North Channel | Marsh Sparrow | 8 | accepted
North Channel | Turtle Nest | 2 | conservation review
South Dunes | Cordgrass | 20 | accepted
South Dunes | Fiddler Crab | 0 | correction required
South Dunes | Rail | 5 | verify

Summary
5 observations: 2 accepted, 1 verify, 1 conservation review,
1 correction required
Accepted count total: 28
North Channel: 8
South Dunes: 20
```

The displayed output is hand-checked from the fixed fixture. It is an example
of the values a host could render from the returned assessment; this package
does not provide a command-line program.

## Review rules

- An observation with a count of zero needs correction, before any species or
  confidence rule is considered.
- A Turtle Nest observation always needs conservation review when its count is
  nonzero, including when its confidence is low.
- Another low-confidence observation needs verification.
- A remaining high-confidence observation is accepted.
- Accepted counts are summed by transect. Other review statuses do not
  contribute to accepted totals.
- A survey contains at most eight observations, and every slot below its
  declared count must be present. The review returns an error before indexing
  past the fixed array or returning a partial assessment. Accepted totals that
  exceed `U32` also return an error; a total exactly equal to `U32_MAX` fits.

`model.sai` defines the observation and result records. `fixtures.sai` contains
the fixed survey and boundary cases. `rules.sai` applies the classification
precedence, `reviewer.sai` validates the bounded input and calculates the
summary, and `report.sai` maps enum values to display labels. `lib.sai`
exposes the example assessment and source-level regression tests.

## Assumptions and limits

All locations, species, confidence labels, and observations are fictional.
The rules demonstrate deterministic review precedence; they are not wildlife,
public-safety, or conservation guidance. The package does not ingest field
data, estimate a population, change a conservation status, persist results, or
connect to monitoring equipment.

## Package and compiler status

This is a pure `wasm32-core` library with fixed in-memory fixtures and no
external dependencies or I/O. The source uses records, enums, fixed arrays,
loops, `Option`, `Result`, and `match`. The current reference compiler
implements only part of Stage 0 checking and does not complete this sample's
multi-module and `@derive` pipeline; `sai test` is also not yet implemented.
No successful semantic check, build, or test execution is claimed.
