# Tool Library Loan Scheduler

This fixed-fixture planner assigns community tool-loan requests to the days
they can be supported. It accounts for copies already on loan, maintenance,
safety badges, members' concurrent-loan limits, duplicate overlapping requests,
and each tool's maximum loan length.

Requests are evaluated in queue order. A request is scheduled only when the
member is known, the tool is known, its loan window fits the seven-day horizon,
the requested duration is within the tool limit, its ready, maintenance, and
currently loaned copy counts sum to its total inventory, the member has the
required badge, the request does not duplicate an earlier scheduled request,
the member's loan limit is respected for every day, and a copy is available on
every day of the loan. The first failed check determines the displayed hold
reason. A held request is not automatically moved to another date.

```text
tool-library summary
Tool library reservations
16 requests; 6 scheduled; 10 held
Held by reason
  no continuous stock: 3
  loan exceeds tool limit: 1
  missing safety badge: 1
  duplicate overlapping request: 1
  member loan limit: 1
  unknown member: 1
  invalid loan window: 1
  unknown tool: 1

tool-library requests
#1 Eli — cordless drill, Monday for 2 days: scheduled
#2 Aiko — circular saw, Monday for 2 days: held (no continuous stock)
#3 Chika — cordless drill, Wednesday for 2 days: scheduled
#4 Dev — circular saw, Tuesday for 2 days: scheduled
...
```

## Commands

- `summary` counts scheduled requests and held requests by reason.
- `requests` prints each request in queue order and its decision.
- `calendar` prints the unreserved copy count for each tool on each day.

The fixture uses days `0` through `6` for Monday through Sunday. A return day
is exclusive: a copy returned on Wednesday is available Wednesday. Current
loans are included in the baseline inventory and member-concurrency counts;
maintenance copies are excluded from usable inventory. Requests reserve one
copy and one member loan for each day in `[pickup_day, pickup_day +
duration_days)`. Fixture rows are stored in ascending `queue_number`; that row
sequence is the allocation order. The planner does not sort or validate
arbitrary input.

## Review required

- Confirm that a hold request should be rejected with its first failed reason
  instead of searching later pickup dates automatically. This keeps the
  allocation deterministic and makes the requested window explicit, but a
  real tool library may prefer a waitlist.
- Confirm the hold-reason precedence: unknown member, unknown tool, invalid
  window, overlong loan, inconsistent inventory, missing badge, overlapping
  duplicate, member loan limit, then unavailable stock. This surfaces bad
  identifiers and request/configuration problems before resource contention.
  For example, request #13 is overlong and lacks a saw badge; the duration
  reason is shown first.
- Confirm that badge checks, loan caps, and repair status are sufficient for
  this illustrative lending policy. The sample does not model deposits,
  opening hours, late-return penalties, or safety training expiry.
- The fixed fixture assumes current-loan rows, ready-copy counts, and repair
  counts are mutually consistent. The planner checks that copy totals balance
  before allocating a request for a tool, but it does not validate malformed
  current-loan rows such as unknown member IDs or return days outside the
  horizon.

## Compiler status

This is an illustrative multi-module command-line package. Its fixed fixture,
reservation rules, and printed arithmetic have been reviewed manually. The
current CLI reports `sai check` as unimplemented at Stage 0; the process
dependency and `@derive` also exceed implemented compiler support. No
successful semantic or runtime check is claimed.
