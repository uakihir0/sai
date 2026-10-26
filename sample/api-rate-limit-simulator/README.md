# API Rate Limit Simulator

This pure `wasm32-core` library replays a fixed, time-ordered request trace
against two registered clients. It applies a separate fixed-window request
budget to each client and records allowed, rate-limited, and unknown-client
outcomes. It makes no network calls and reads no clock or client registry.

## Hand-checked fixture

Each client may make two requests in each aligned ten-second window. Windows
are half-open: `[0, 10)`, `[10, 20)`, and so on. A request exactly at the
boundary starts the next window. The fixture is processed in nondecreasing
time order; requests with the same timestamp follow their array order.

| Request | Client | Time (s) | Decision | Remaining | Retry time | Retry after |
|---:|---|---:|---|---:|---:|---:|
| 9001 | Ivy | 0 | Allowed | 1 | — | — |
| 9002 | Moss | 0 | Allowed | 1 | — | — |
| 9003 | Ivy | 3 | Allowed | 0 | — | — |
| 9004 | Ivy | 9 | Rate limited | 0 | 10 | 1 s |
| 9005 | Moss | 9 | Allowed | 0 | — | — |
| 9006 | Ivy | 10 | Allowed | 1 | — | — |
| 9007 | Moss | 10 | Allowed | 1 | — | — |
| 9008 | Ivy | 11 | Allowed | 0 | — | — |
| 9009 | Ivy | 19 | Rate limited | 0 | 20 | 1 s |
| 9010 | Moss | 19 | Allowed | 0 | — | — |
| 9011 | Ivy | 20 | Allowed | 1 | — | — |
| 9012 | Pine | 20 | Unknown client | 0 | — | — |

The trace produces **9 allowed**, **2 rate-limited**, and **1 unknown-client**
request. Request 9004 is denied only for Ivy: Moss still has an independent
budget at the same time. At 10 seconds both registered clients enter a fresh
window. A blocked attempt does not consume another admission; each retry time
is the end of that client's current window. Ivy finishes in `[20, 30)` with
one admission used. Moss finishes in `[10, 20)` with both admissions used.

## Modules

- `src/model.sai` defines the fixed request trace, client buckets, decisions,
  and report records.
- `src/policy.sai` defines the window width, request cap, window start, and
  retry calculation.
- `src/fixtures.sai` provides the two registered clients and twelve requests.
- `src/limiter.sai` looks up each client, resets its bucket at a new window,
  and records the decision while preserving other clients' state.
- `src/lib.sai` exposes the fixture simulation and source-level assertions for
  the result counts, boundary resets, blocked retries, and unknown client.

## Review required

- The sample uses an epoch-aligned fixed window, not a sliding window or token
  bucket. Fixed windows can admit a burst on either side of a boundary; confirm
  this is the intended teaching policy before reusing the pattern.
- A request at the exact window end belongs to the new window. Requests at
  equal timestamps are resolved in input order, so the first two registered
  client requests are admitted and later ones are limited.
- Only the two pre-registered client IDs receive buckets. An unknown ID is
  reported and denied without allocating state. A real service must define
  client identity and registration separately from untrusted request fields.
- The ten-second window and two-request cap are invented fixture values, not
  operational recommendations. Retry time is the window end; the example does
  not implement HTTP headers or transport behavior.
- Input timestamps are assumed to be bounded and nondecreasing. The simulator
  does not detect time reversal, clock skew, integer overflow, duplicate IDs,
  or malformed external requests.
- The fixture models a single in-memory process. It does not coordinate
  concurrent handlers or enforce a quota across replicas, regions, credentials,
  or API keys with different weights.

## Formatting and compiler status

Run `sai fmt --check sample/api-rate-limit-simulator` from the repository root
to check parsing and formatting. The result table is hand-checked. Assertions
document intended behavior and have not executed. The current compiler's Stage 0
`sai check` and `@derive` gates prevent semantic type checking, and Stage 3
`sai test` is not implemented, so this sample does not claim to build or run.
