# Webhook Delivery Audit

Webhook Delivery Audit replays a fixed set of HTTP response scripts to show how
a sender records retries, final outcomes, and duplicate requests. It performs
no network requests. The response codes and timestamps are fixtures, so every
state transition can be checked by hand.

```text
9 requests
4 delivered, 2 permanent failures, 1 retry exhausted, 2 duplicates suppressed
14 total attempts

401 invoice.paid       delivered          503@10, 503@11, 204@13
402 stock.low          permanent failure  429@20, 422@21
403 invoice.paid       delivered          201@22
404 invoice.paid       duplicate          no attempts
405 batch.closed       retry exhausted   503@30, 503@31, 500@33, 503@37
406 account.changed    delivered          408@40, 202@41
407 report.ready       permanent failure  404@50
408 stock.low          delivered          202@55
409 stock.low          duplicate          no attempts
```

## Delivery rules

- A 2xx response succeeds. HTTP 408, 429, and 500–599 are retryable. Every
  other response is a permanent failure.
- Each request gets at most four attempts. After the first, second, and third
  retryable responses, the next attempt is scheduled 1, 2, and 4 minutes later.
- A repeated `(endpoint, idempotency key)` pair in the same input batch is
  suppressed after its first occurrence, whether that request succeeded or
  failed. The same key at a different endpoint is a separate delivery.
- The response script must contain enough responses to reach success,
  permanent failure, or the four-attempt limit. A script that ends earlier is
  reported as incomplete, and the audit rejects it.
- Attempt timestamps are derived from the request's creation minute. This
  sample does not model wall-clock delays, network timeouts, or queue
  concurrency.

The fixture deliberately includes success after two transient failures, a
rate-limit response followed by a permanent client error, an immediate
success, duplicates after both successful and failed requests, a four-attempt
outage, success after a timeout, a permanent 404, and the same idempotency key
sent to a different endpoint. The expected audit contains 4 successful
requests, 2 permanent failures, 1 exhausted request, and 2 suppressed
duplicates across 14 attempts.

## Project structure

- `model.sai` defines requests, idempotency keys, attempt histories, outcomes,
  and summaries.
- `data.sai` provides the nine fixed request and response fixtures.
- `policy.sai` classifies HTTP responses and defines the backoff schedule.
- `scheduler.sai` replays each request, records every attempt, and suppresses
  repeated endpoint/key pairs.
- `evaluation.sai` validates request coverage, attempt ordering, backoff times,
  response transitions, and duplicate handling before producing a summary.
  Invalid or incomplete audits are rejected without partial totals.
- `lib.sai` exposes the fixture audit and keeps focused SAI test declarations
  beside the public API.

This advanced pure-library example combines fixed arrays, nested scans,
state-machine transitions, bounded retries, deterministic timing, audit
invariants, and structured results. The implementation is illustrative and is
not claimed to build or run with the current compiler: the full package,
`@derive`, array mutation, loops, and test runner remain outside its verified
pipeline. `sai fmt --check sample` verifies parsing and formatting only; the
fixture output above is hand-checked.
