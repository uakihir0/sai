# Bounded Protocol Model Checker

This pure library explores every reachable state in a tiny two-client lease
protocol. Breadth-first search checks that both clients are never holding the
lease at once. When it finds a violation, it reconstructs a shortest,
deterministic action trace from predecessor links.

The fixtures exercise four distinct outcomes:

- A server that reserves the lease when it sends a grant is reported
  `ProvenSafe` for this finite model.
- A server that waits until grant delivery to reserve the lease can issue two
  grants while the owner still appears free. The shortest counterexample is
  `RequestA, RequestB, SendGrantA, SendGrantB, DeliverGrantA, DeliverGrantB`.
- If a release message can be lost, an unfinished request can become stuck
  behind a lease the server still considers occupied. The checker reports a
  reachable `Deadlock` and returns a shortest trace to it.
- If the fixed state table fills before the graph is exhausted, the result is
  `BoundExceeded`; it never reports an incomplete search as safe.

`Deadlock` is returned only after the whole graph has been explored without a
safety counterexample. If a deadlock was seen but a later state does not fit,
`BoundExceeded` takes precedence. Counts in early counterexample or bound
results describe only the explored prefix.

## Modules

- `src/model.sai` defines the 128-state/trace capacity, protocol state,
  configuration, actions, trace, and check outcomes.
- `src/protocol.sai` lists the finite atomic transitions and the mutual
  exclusion invariant.
- `src/checker.sai` performs bounded BFS, state deduplication, deadlock
  detection, and predecessor-based trace reconstruction.
- `src/fixtures.sai` provides reliable, vulnerable, and lossy configurations.
- `src/lib.sai` exposes the checker and contains regression tests.

States include the server's owner, both pending requests, grants in flight,
client-held leases, and releases in flight. Search state capacity and trace
capacity are both fixed at 128 entries. The action order is fixed, so equal
length witnesses are selected reproducibly.

## Review required

The model is a teaching example and needs protocol review before it is used to
reason about a real implementation:

- Each transition is one atomic event. Network reordering, duplicated
  messages, clocks, lease expiry, retries, and client crashes are outside the
  model. Release-message loss is represented by one explicit transition.
- `ReserveWhenReceived` represents a server that fails to reserve ownership
  before sending a grant, while clients trust every delivered grant. It is a
  deliberately vulnerable protocol abstraction, not a claim about any
  particular implementation.
- A client may submit another request after locally beginning a release even
  while that release message is still in flight. This exposes a pending request
  when a release is lost; review this overlap assumption for any intended
  protocol.
- `ProvenSafe` means the entire finite state graph for the selected
  configuration was explored without violating mutual exclusion. It does not
  prove liveness, fairness, or safety for behaviors omitted from this model.
- The fixed limits and two-client protocol keep exhaustive search small. A
  larger model that reaches the state limit must be reported as
  `BoundExceeded`, even if no violation has been found yet.

## Formatting and compiler status

Run the source-format check with:

```sh
sai fmt --check sample/bounded-protocol-model-checker
```

The package is a dependency-free `wasm32-core` library. The sample includes
`@derive` and embedded `test` declarations; the current reference compiler
stops `sai check` at the unimplemented Stage 0 attribute gate and `sai test`
at the unimplemented Stage 3 test runner. The expected traces above describe
the model's intended behavior and are not compiler-generated output.
