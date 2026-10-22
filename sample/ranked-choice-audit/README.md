# Ranked Choice Audit

Ranked Choice Audit is a small, fixed-capacity example of auditing ranked
ballots one round at a time. It uses three named candidates with stable IDs:
Maya (`10`), Niko (`20`), and Oren (`30`). Ballots rank any subset of the
candidates, without repeating a candidate.

Each round assigns each ballot to its highest-ranked candidate who is still
continuing. A ballot with no continuing preference is exhausted. The majority
threshold is computed from the active ballots in that round:

```text
majority_required = floor(active_ballot_count / 2) + 1
```

The threshold therefore requires strictly more than half of the active ballots.
The audit records the active and exhausted counts separately; for every round,
their sum equals the number of input ballots. Exhausted ballots are not counted
again as new ballots in later rounds.

## Hand-checked fixtures

`tied_first_round_with_exhaustion()` uses six ballots:

| Ballots | First preference | Next preference |
| --- | --- | --- |
| 1, 2 | Maya | Niko / Oren |
| 3, 4 | Niko | Maya / Oren |
| 5 | Oren | Maya |
| 6 | Oren | none |

All three candidates receive two votes in round 1. No one reaches the required
four votes. The illustrative tie rule eliminates Oren because candidate ID 30
is the highest ID among the candidates tied at the minimum. In round 2, ballot
5 transfers to Maya, ballot 6 is exhausted, and the tally is Maya 3, Niko 2,
Oren 0. There are five active ballots, one exhausted ballot, and a threshold of
three, so Maya is selected.

`exactly_half_is_not_a_majority()` starts Maya at two of four active votes. The
required threshold is three, so the first round eliminates Oren from a tie at
the minimum with Niko. Oren's ballot transfers to Maya, producing Maya 3 and
Niko 1 in round 2.

`first_round_majority()` gives Maya three of five active votes. The threshold is
three, so the audit ends after its first round. `no_ballots()` demonstrates
the empty-input boundary: the result has no winner and reports zero active and
zero exhausted ballots.

## Tie policy and scope

When multiple continuing candidates share the lowest vote count, this sample
eliminates the one with the highest numeric candidate ID. This rule makes the
fixture deterministic and easy to hand-check. It is illustrative only; it is
not election law, legal guidance, or a recommendation for real elections.
Real election procedures and tie rules depend on the applicable jurisdiction
and should not be inferred from this sample.

`model.sai` defines the bounded election, ballot, and audit records.
`validate.sai` checks candidate and ballot counts, packed slots, unique IDs,
known candidate references, and non-repeating preferences before tallying. `audit.sai`
computes each round and records the tally before eliminating a candidate.
`fixtures.sai` contains the hand-checkable inputs, while `lib.sai` exposes
example entry points and source-level tests.

## Package and compiler status

This is a pure `wasm32-core` library with room for at most three candidates,
eight ballots, and three preferences per ballot. It has no command-line
interface, input parser, file or network I/O, or dynamic-sized collections.
The candidate and ballot counts are checked before their fixed arrays are
indexed.

The source includes SAI test declarations for the listed tally and validation
cases, but they have not been executed in this environment. This is an
illustrative package, not a build or runtime guarantee; the current compiler
and package pipeline have not been verified for this sample.
