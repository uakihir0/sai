# Bounded DNS Cache Resolver

This fixed-fixture `wasm32-core` library replays DNS A questions against a
small in-memory zone and a four-entry cache. It follows CNAME records, computes
the effective TTL across a chain, distinguishes positive, NXDOMAIN, and NODATA
responses, detects alias loops, and evicts the least recently used cache entry.
It makes no network calls and reads no wall clock.

## Hand-checked fixture

Names are stable enum values mapped to these fictional records:

| Name | Owner | Data | TTL |
|---|---|---|---:|
| `Alpha` | `alpha.example.test` | A `192.0.2.101` | 8 s |
| `Www` | `www.example.test` | CNAME `Alpha` | 12 s |
| `Beta` | `beta.example.test` | A `192.0.2.202` | 5 s |
| `Docs` | `docs.example.test` | CNAME `Edge` | 4 s |
| `Edge` | `edge.example.test` | A `192.0.2.30` | 10 s |
| `Empty` | `empty.example.test` | Name exists, no A record | negative TTL 6 s |
| `LoopA`, `LoopB` | loop-a / loop-b.example.test | CNAME to each other | 20 s each |
| `LongA` through `LongF` | long-a through long-f.example.test | Five CNAME edges, then A `192.0.2.40` | 20 s each |
| `Missing`, `Unlisted` | absent names | NXDOMAIN | negative TTL 6 s |

Address IDs are compact stand-ins for documentation-only IPv4 addresses. The
clock is monotonic integer seconds. The fixture contains 16 questions: repeated
positive and negative queries, a CNAME cycle, exact TTL-boundary queries at
seconds 8, 9, and 18, same-time queries at second 10, and cache evictions.

| ID | Name | Time | Result | Cache | Expiry / eviction |
|---:|---|---:|---|---|---|
| 1 | `Www` | 0 | `192.0.2.101` | Miss | 8 |
| 2 | `Www` | 1 | `192.0.2.101` | Hit | 8 |
| 3 | `Beta` | 2 | `192.0.2.202` | Miss | 7 |
| 4 | `Missing` | 3 | NXDOMAIN | Miss | 9 |
| 5 | `Empty` | 4 | NODATA | Miss | 10 |
| 6 | `Www` | 5 | `192.0.2.101` | Hit | 8 |
| 7 | `Docs` | 6 | `192.0.2.30` | Miss | 10; evicts `Beta` |
| 8 | `Missing` | 7 | NXDOMAIN | Hit | 9 |
| 9 | `Www` | 8 | `192.0.2.101` | Miss | 16; exact-boundary expiry |
| 10 | `Missing` | 9 | NXDOMAIN | Miss | 15; exact-boundary expiry |
| 11 | `Alpha` | 10 | `192.0.2.101` | Miss | 18 |
| 12 | `Docs` | 10 | `192.0.2.30` | Miss | 14; its old entry expired at 10 |
| 13 | `LoopA` | 11 | alias loop | Miss | not cached |
| 14 | `Unlisted` | 12 | NXDOMAIN | Miss | 18; evicts `Www` |
| 15 | `Unlisted` | 13 | NXDOMAIN | Hit | 18 |
| 16 | `Unlisted` | 18 | NXDOMAIN | Miss | 24; exact-boundary expiry |

The report contains **8 positive**, **6 NXDOMAIN**, **1 NODATA**, and **1 alias
loop** response, with **4 cache hits**, **12 cache misses**, and **2 LRU
evictions**. The positive answer for `Www` expires at second 8 because the
CNAME TTL of 12 seconds is longer than `Alpha`'s 8-second A TTL. At second 8,
`expires_at <= now` invalidates that entry before lookup. The negative entry
for `Missing` similarly expires at second 9, and `Unlisted` at second 18.

At question 7, `Beta` is evicted because its access order is oldest even though
other entries were inserted earlier. At question 14, `Www` is the least
recently used remaining name. An alias loop is returned as an uncached finding;
it does not consume a cache slot or evict an unrelated answer.

## Modules

- `src/model.sai` defines the zone rows, questions, outcomes, cache, and report.
- `src/policy.sai` sets all array/time/TTL/hop bounds and the negative TTL.
- `src/zone.sai` provides the finite record table and owner lookup.
- `src/fixtures.sai` provides the 16-question deterministic trace.
- `src/resolver.sai` follows bounded CNAME chains and calculates effective TTLs.
- `src/cache.sai` expires entries before lookup and inserts with LRU replacement.
- `src/engine.sai` validates query time, replays the trace, and records every
  decision, cache occupancy, and eviction.
- `src/lib.sai` exposes the fixture run and source-level regression assertions.

## Concepts and limits

The sample demonstrates fixed arrays, enums, records, `Option`, linear owner
lookup, bounded graph traversal, alias-cycle detection, negative answers,
absolute expiration, deterministic LRU replacement, and a stable audit trail.
The cache is process-local, has four entries, accepts at most 16 questions,
limits query time to 1,000 seconds and record TTLs to 100 seconds, and follows
at most four CNAME edges. An invalid or reversed timestamp is logged without
changing the cache or its monotonic clock.

This is an educational resolver model, not an RFC-complete DNS implementation.
It supports only A questions and one fixed zone; it does not parse wire packets,
perform recursive or authoritative network lookups, model DNSSEC, bailiwick,
SOA-derived negative TTLs, delegation, search suffixes, multiple RRsets,
concurrent clients, cache sharding, or stale serving. Each cache entry stores a
flattened answer under the original question name, with the minimum TTL across
the CNAME path and terminal record. NXDOMAIN/NODATA use a single explicit
six-second fixture TTL rather than SOA semantics. The fixed records are
validated only by construction, and cache LRU uses the question's array order
rather than elapsed time.

## Review required

- The cache flattens a complete CNAME-to-A answer under the original A name.
  Real DNS caches usually retain CNAME and terminal RRsets independently and
  can share those cached records across aliases. The flattened model is chosen
  to keep a bounded example auditable; review that simplification before using
  it as a cache implementation template.
- Expiry is exclusive: a record with `expires_at_seconds == now` is stale and
  is removed before lookup. This follows half-open TTL validity and is tested
  at seconds 8, 9, and 18; deployments should verify how their clocks and TTL
  granularity represent this boundary.
- Both NXDOMAIN and NODATA use one explicit six-second negative TTL. DNS
  negative caching derives its lifetime from SOA data and can differ by
  response. This fixture value is illustrative, not a resolver recommendation.
- CNAME traversal has a four-edge limit. Cycles and longer chains fail closed
  and are not cached; no alias loop is silently turned into NXDOMAIN. The
  `LongA` fixture chain exceeds the limit, while the main query trace exercises
  cycle detection.
- LRU ties are resolved by lower cache-array index. The fixture's access-order
  counter is the query's position in the fixed array, which is deterministic
  for same-second questions but does not model concurrent access races.
- Time must be nondecreasing and no greater than 1,000 seconds. Reversed and
  out-of-range timestamps are rejected before cache expiry so invalid input
  cannot move the cache clock forward or backward.

## Formatting and compiler status

Run `sai fmt --check sample/bounded-dns-cache-resolver` from the repository root.
The expected table and assertions are hand-checked; they are not compiler output.
The current reference compiler stops `sai check` at the unimplemented Stage 0
attribute gate and `sai test` at the unimplemented Stage 3 test runner, so this
sample does not claim to build or run.
