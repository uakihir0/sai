# Corpus Search Ranker

Corpus Search Ranker ranks a fixed set of short documents for a whitespace-
separated query. It compares tokens using ASCII case-insensitive equality,
weights each title match three times more than a body match, and orders equal
scores by ascending document ID.

For the query `river water`, the fixed corpus produces these results:

| Rank | Document ID | Score | Title matches | Body matches |
|---:|---:|---:|---:|---:|
| 1 | 101 | 5 | 1 | 2 |
| 2 | 202 | 5 | 1 | 2 |
| 3 | 303 | 1 | 0 | 1 |
| 4 | 505 | 1 | 0 | 1 |

The stable ID tie-break makes both ties independent of fixture order. The
example rejects queries longer than 64 UTF-8 bytes before tokenization, then
stops counting after it finds a ninth term. It rejects empty queries and
queries with more than eight terms before scanning the corpus. A query term is
a complete token separated by SAI whitespace; punctuation is preserved, and
case folding is ASCII-only. Repeated query terms contribute repeatedly to a
score.

The project is a pure `wasm32-core` library with a five-document corpus and a
fixed-size result array. It demonstrates borrowed string iteration, bounded
input validation, weighted scoring, and deterministic insertion ranking
without allocation or external packages.

`sai fmt --check sample` verifies source formatting. The current compiler still
gates semantic checking on the Stage 0 `sai check` implementation; `sai test`
is a Stage 3 command that is not yet implemented. The test declarations and
hand-calculated scores therefore have not been executed by SAI.
