# Swiss Tournament Auditor

Swiss Tournament Auditor checks a fixed three-round tournament, calculates
match points and Buchholz tie-breaks, and produces a deterministic standings
table. It reports malformed fixtures before scoring them.

The fixture uses six fictional players and three rounds of three games each.
Every player plays once per round; this illustration has no byes. A win earns
3 match points, a draw earns 1, and a loss earns 0. Buchholz is the sum of the
final match points of the opponents a player faced. Standings sort by match
points descending, Buchholz descending, then player ID ascending.

The hand-checked fixture result is:

| Rank | Player | Match points | Buchholz |
|---:|---|---:|---:|
| 1 | Eli | 6 | 8 |
| 2 | Cora | 5 | 16 |
| 3 | Ada | 5 | 13 |
| 4 | Dev | 5 | 10 |
| 5 | Bex | 3 | 11 |
| 6 | Finn | 0 | 14 |

Before scoring, the auditor checks the fixed match count, present match slots,
unique player IDs, valid round numbers, known players, self-pairings, repeated
opponents, and one game per player per round. Exactly three games per round
follows from nine games across three rounds and at most three disjoint games
per round. Fixtures exercise every explicit validation error, including a
missing slot, duplicate player ID, invalid round, self-pairing, unknown
player, repeated pairing, double booking, and a declared-count mismatch.

This is a pure `wasm32-core` library over fixed in-memory data. It demonstrates
validation across related records, opponent-dependent tie-breaks, and stable
ordering without a tournament service or external package. The scoring and
pairing rules are teaching choices, not a claim to implement any federation's
Swiss-system regulations.

`sai fmt --check sample` checks source formatting. The current compiler still
gates semantic checking on the Stage 0 `sai check` implementation; `sai test`
is a Stage 3 command that is not yet implemented. The test declarations and
hand-calculated standings therefore have not been executed by SAI.
