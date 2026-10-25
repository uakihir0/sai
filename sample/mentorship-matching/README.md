# Mentorship Matching

This pure SAI library matches five fictional mentees to three fictional
mentors. Mentees propose in a deterministic order; each mentor tentatively
holds up to their stated capacity and keeps the most preferred acceptable
mentees seen so far. A displaced or rejected mentee continues at their next
choice. The report retains every proposal and the final matched and unmatched
mentees.

## Hand-checked expected result

These are manual fixture results, not output from a successful build or run.

| Round | Mentee | Mentor | Result |
|---:|---|---|---|
| 1 | Aki Tanaka | Mira Chen | Held |
| 2 | Bea Sato | Niko Park | Held |
| 3 | Chen Ito | Mira Chen | Replaced Aki Tanaka |
| 4 | Aki Tanaka | Niko Park | Held |
| 5 | Daya Mori | Mira Chen | Skipped: climate topic is ineligible |
| 6 | Daya Mori | Niko Park | Replaced Bea Sato |
| 7 | Bea Sato | Oren Ali | Held |
| 8 | Eli Kato | Mira Chen | Rejected |
| 9 | Eli Kato | Niko Park | Rejected |
| 10 | Eli Kato | Oren Ali | Rejected |

Final assignments, shown in mentee input order:

| Mentee | Mentor | Mentee's rank | Mentor's rank |
|---|---|---:|---:|
| Aki Tanaka | Niko Park | 2 | 2 |
| Bea Sato | Oren Ali | 2 | 1 |
| Chen Ito | Mira Chen | 1 | 1 |
| Daya Mori | Niko Park | 2 among listed choices; first eligible choice | 1 |
| Eli Kato | Unmatched | — | — |

The report records four pairs, one unmatched mentee, ten proposal turns, two
replacements, one ineligible choice, and three rejections. Aki's first choice
is Mira, who keeps Chen over Aki. Bea's first choice is Niko, who keeps Daya
and Aki over Bea. Chen gets their first choice; Daya's first listed mentor,
Mira, is ineligible, so Niko is Daya's first eligible choice. Eli exhausts all
three compatible options without displacing an incumbent.

Under the fixture's strict preference lists and acceptability rule, the result
is stable: there is no eligible pair who both rank each other and would each
prefer one another over their final assignment (or unmatched status). For
example, Aki and unmatched Eli both prefer Mira, but Mira ranks Chen above Aki
and Eli, and Aki above Eli; Bea prefers Niko over Oren, but Niko ranks both
current mentees above Bea. Eli is ranked below every incumbent at all three
mentors.

## Algorithm and bounds

The matcher first validates all active roster entries and preference
references. An invalid input returns an error and an empty report before it
records any proposal. It then repeatedly selects the first currently
unmatched mentee, in fixture order, who has a choice left. The mentee proposes
to that choice and advances their cursor immediately. Each sequential round
contains exactly one proposal. An ineligible choice is recorded as skipped. A
mentor who has room holds the proposer; a full mentor replaces their
least-preferred held mentee only if the new proposer ranks higher. Otherwise
the proposal is rejected. Each log entry stores its round, both IDs, and
outcome. The final pairs also carry each side's preference rank.

Preference lists are strict and contain no repeated IDs. A pair is acceptable
only if the mentee lists the mentor, the mentor lists the mentee, and the
mentor's topic flags include the mentee's topic. A choice not listed by the
mentor is rejected. Capacities may be zero; a capacity above the fixed
five-mentee bound is invalid. ID uniqueness is checked within each side, so a
mentee ID may equal a mentor ID.

There are at most five mentees, three mentors, and three mentee choices each,
so the algorithm stores at most 15 proposal turns. If `m` is the mentee count,
`h` the mentor count, and `p` the number of proposal turns, the implementation
runs in `O(m² + h² + m × h² + h × m² + p × (h + m²) + m × (h + m))` time.
This includes duplicate-ID scans, preference-reference and duplicate-choice
validation, each proposal's mentor/rank/held-capacity searches, and assembling
the final pairs. It uses bounded linear searches rather than precomputed rank
maps. Since `p ≤ m × h`, at the fixture limits there can be at most 15 turns.
Storage is `O(m × h + m)` for the proposal log and current assignment state.

The order in which unmatched mentees are selected is fixed so the audit log
is reproducible. It does not change the deferred-acceptance result under the
documented complete choice processing, strict rankings, and acceptability
rule. The fixture deliberately includes an ineligible listed option, two
replacements, full-mentor rejections, and an unmatched mentee.

## Project structure

- `src/model.sai` defines the bounded input, error, proposal, pair, and report
  types.
- `src/data.sai` contains the five mentees, three mentors, their topic
  compatibility, capacities, and ordered preferences.
- `src/policy.sai` provides ID lookup, eligibility, and rank lookup.
- `src/validate.sai` checks roster bounds, nonzero and unique IDs, valid
  preference counts, known preference references, and duplicate choices.
- `src/matching.sai` implements mentee-proposing deferred acceptance, logs each
  proposal, and produces matched and unmatched results.
- `src/lib.sai` exposes `fixture_report()` and regression assertions for the
  hand-checked assignment, preference ranks, proposal sequence, rejections,
  replacements, eligibility skip, capacity, and invalid input.

## Assumptions and limitations

All people and preferences are fictional. Topic compatibility is a fixed
boolean flag on each mentor, and preferences are supplied as trusted input.
There are no ties, changing availability, mentor or mentee withdrawals,
multiple program tracks, protected attributes, time windows, or persistence.
The sample records a mentor's proposal outcome but does not model an actual
introduction or consent step. An unmatched result means only that the
configured acceptable choices were exhausted under the configured capacity
and ranking rules; it does not assess a person's worth or eligibility for
other programs.

## Review required

- The mentee-proposing variant gives mentees the proposer advantage in
  many-to-one deferred acceptance. The example chooses it to demonstrate
  displaced mentees continuing down their lists; a real program must decide
  which side proposes and how that choice is approved and explained.
- The strict, incomplete preference lists make an unlisted partner
  unacceptable. A real program must decide whether missing preferences mean
  unacceptable, unknown, or a prompt for follow-up.
- Capacity and topic flags are hard gates. Their source, exception process,
  accommodations, and ownership are outside this example and require
  program-specific review.
- Stable means only that no mutually acceptable blocking pair exists under
  these supplied rankings. It does not imply fairness, suitability, consent,
  legal compliance, or a desirable outcome for the people represented.
- The fixed fixture order is used only to make proposal logs reproducible.
  A deployed system should document whether any queue-order rule may affect
  observability or participant trust.

## Package and compiler status

The package targets `wasm32-core`, has no external dependencies, and requests
no runtime capabilities. `sai fmt --check` parses and formats the source, but
that is not a semantic or runtime check. The current compiler's Stage 0
`sai check` and `@derive` feature gates prevent full semantic compilation,
and Stage 3 `sai test` is not implemented. The expected proposal log, stable
assignment, and assertions are hand-checked; no successful check, test, build,
or run is claimed.
