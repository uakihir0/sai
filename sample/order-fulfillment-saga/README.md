# Order Fulfillment Saga

This dependency-free library replays six small, fictional order workflows from
fixed event sequences. Each sequence moves through inventory reservation,
payment capture, and shipment creation. A failed later step starts compensating
actions in reverse order: refund the captured payment, then release reserved
inventory. The program only interprets recorded example events; it does not
contact inventory, payment, or shipping services.

The table is a hand-calculated expectation for the fixtures, not program output:

```text
Order  Final state   Forward outcome             Compensation / audit
7001   Completed     payment retry succeeded    1 duplicate arrival suppressed
7002   Compensated   shipment retry budget used  refund, then inventory release
7003   NeedsReview   shipment rejected           refund failed; payment and stock remain committed
7004   Compensated   payment rejected            inventory released; no refund attempted
7005   Rejected      inventory reservation failed no later action was attempted
7006   NeedsReview   event ID payload conflict    conflicting arrival retained in the log

Summary: 1 completed, 2 compensated, 1 rejected, 2 needing review
Events: 22 arrivals, 20 actions applied, 1 exact duplicate, 1 rejected event
Retries: 4 retryable failures across 20 applied stage attempts
```

## Replay rules

- Event IDs are unique within one order's saga. Re-delivering an identical event
  is retained in the audit log and does not apply its action twice. Reusing an
  ID for a different payload is retained and places the saga in `NeedsReview`.
- Order IDs are unique within a batch. A later case with an already-seen order
  ID invalidates the batch, is retained as a held result, and has none of its
  event actions replayed.
- Each step has a stable operation key. A retry must keep the same key and use
  the next consecutive attempt number. The fixed retry budget is two attempts
  per step.
- Inventory must be reserved before payment, and payment must be captured
  before shipment. A permanent failure or exhausted retry budget stops forward
  progress.
- A failed payment requires only inventory release. A failed shipment requires
  a payment refund followed by inventory release. `Compensated` is reported
  only after every required compensation succeeds.
- Failed compensation produces `NeedsReview` and preserves the recorded state
  of any side effect that was not successfully reversed. The result reports the
  original failure, compensation failure, attempts, duplicate arrivals,
  rejected arrivals, and event sequence.
- Fresh events after a terminal state are preserved and flagged for review.

These are teaching policies for a bounded fixture, not a payment protocol or
distributed transaction guarantee. Real systems need durable journals,
transactional outboxes, authentication, service-specific idempotency
contracts, deadlines, concurrency handling, and operational recovery.

## Modules and tests

- `src/model.sai` defines the event, order case, status, failure, and report
  types, with fixed event and batch capacities.
- `src/fixtures.sai` provides the six event scripts, including an exact
  duplicate, a conflicting duplicate ID, an exhausted forward retry, a
  successful compensation chain, and a partial compensation failure.
- `src/replay.sai` validates event identity, step order, attempt numbering, and
  operation-key stability before applying each simulated receipt.
- `src/audit.sai` replays the cases and aggregates deterministic counts.
- `src/lib.sai` exposes the fixture audit and declares ten Stage 3 tests for
  successful, compensated, rejected, duplicate, conflicting, and partial
  failure paths, plus duplicate-order and attempt-number guards.

## Compiler status

The manifest targets `wasm32-core` and has no external package or runtime
dependencies. Ten embedded `test` declarations cover the intended behavior,
but their expectations have not been executed. `@derive` and generic `Array`
operations require later semantic and code-generation stages; test execution
requires Stage 3. The current reference compiler has not built or run this
project. The table above is a manual calculation.

Each order is limited to eight arrivals, and the batch to six orders. A
declared event count above capacity is held for review before replay. An empty
event slot inside a declared count is also held at that point.

The checks run for this sample were:

- `sai fmt --check sample/order-fulfillment-saga` passed.
- `sai check -C sample/order-fulfillment-saga` reported that `sai check` and
  `@derive` are not implemented at the current stage.
- `sai test --list -C sample/order-fulfillment-saga` exited with code 1 and
  reported `FEATURE_NOT_IMPLEMENTED` for the Stage 3 `sai test` command, so the
  ten test names could not be listed or run.

## Review required

- Confirm that an order-scoped event ID is a sufficient idempotency boundary
  for this teaching model and that a conflicting payload should stop the saga
  for review.
- Confirm that order IDs must be unique within a batch and that rejecting a
  duplicate case before replay is the right fail-closed response.
- Confirm that two attempts per forward or compensating step are a clear
  deterministic retry policy for this example.
- Confirm that compensation should run in reverse order and that a partial
  refund failure must leave both the captured-payment and reserved-inventory
  facts visible instead of reporting a successful rollback.
- Confirm that `NeedsReview` is an appropriate status for structural event
  problems and failed compensation, with separate fields preserving their
  reason.
