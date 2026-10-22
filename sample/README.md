# SAI samples

These projects illustrate SAI package structure and progressively richer
language features. The levels below are a reading guide, not a guarantee that
the current compiler can build a sample. The output shown in individual
READMEs describes the intended behavior; it is not a claim that the program
has been built or run.

The command-line samples declare the canonical `sai:process` source and `^0.1`
range from the package examples. The `sai:process` Git URL is currently
unavailable, so the dependency cannot currently be fetched from that source.
The package is specified as a toolchain-bundled standard package, so these
manifests need a matching bundled package index or an available published
source repository before a package resolver can fetch them.

| Level | Sample | Purpose and concepts |
|---|---|---|
| Starter | [`hello-world`](hello-world/README.md) | Print the text greeting `Hello, world!`; introduce a command entry point and stdout. |
| Starter | [`greet-cli`](greet-cli/README.md) | Read a name argument and format a personalized greeting. |
| Starter | [`free-shipping-check`](free-shipping-check/README.md) | Decide whether an order meets a free-shipping threshold with a typed comparison. |
| Beginner | [`temperature-converter`](temperature-converter/README.md) | Convert Celsius to Fahrenheit with `F32` parameters, locals, and arithmetic. |
| Beginner | [`mini-task-board`](mini-task-board/README.md) | Split a small task board into modules; use records, enums, command dispatch, and summaries. |
| Beginner | [`profile-check`](profile-check/README.md) | Parse and validate a key-value profile; return structured errors with `Result`. |
| Intermediate | [`reading-log`](reading-log/README.md) | Summarize a fixed book queue with status counts, page totals, and next-book selection. |
| Intermediate | [`market-checkout`](market-checkout/README.md) | Review orders, aggregate repeated stock requests, and calculate discounts and tax. |
| Intermediate | [`library-loan-policy`](library-loan-policy/README.md) | Enforce checkout, hold, and renewal rules with records, enums, and `Result`. |
| Advanced | [`event-planner`](event-planner/README.md) | Apply session capacity and waitlists, then detect conflicts between confirmed sessions. |
| Advanced | [`turn-based-duel`](turn-based-duel/README.md) | Simulate stateful turns with combat rules, mutable state, and a deterministic opponent strategy. |
| Advanced | [`incident-response-console`](incident-response-console/README.md) | Score incidents, apply response deadlines, and identify SLA breaches and escalations. |
| Advanced | [`island-grid-dispatch`](island-grid-dispatch/README.md) | Allocate a constrained one-hour power budget by criticality while preserving a battery reserve. |
| Advanced | [`clinic-shift-roster`](clinic-shift-roster/README.md) | Validate clinician credentials, availability, shift caps, rest periods, and required coverage. |
| Advanced | [`community-food-bank`](community-food-bank/README.md) | Allocate expiring pantry stock across household requests with dietary eligibility and priority rules. |
| Advanced | [`warehouse-fulfillment`](warehouse-fulfillment/README.md) | Reserve date-eligible inventory for prioritized orders, then choose a warehouse and route under cold-chain, delivery-deadline, and lane-capacity constraints. |
| Advanced | [`rail-shuttle-dispatch`](rail-shuttle-dispatch/README.md) | Assign a fixed replacement-bus fleet to disrupted transit services under corridor, passenger-capacity, and wheelchair-space constraints; report unresolved requests deterministically. |
| Advanced | [`weighted-route-finder`](weighted-route-finder/README.md) | Find a minimum-time multi-edge route with Dijkstra's algorithm, stable tie-breaking, and predecessor-based path reconstruction. |
| Advanced | [`access-policy-review`](access-policy-review/README.md) | Evaluate role, action, and resource rules with deny-overrides, default-deny, and stable decision explanations. |
| Advanced | [`package-lock-auditor`](package-lock-auditor/README.md) | Check a fixed dependency lock for missing packages, version-range conflicts, and cycles; produce a stable build order only when the graph is valid. |
| Advanced | [`telemetry-frame-decoder`](telemetry-frame-decoder/README.md) | Validate and decode fixed sensor frames with byte masks, a checksum, and ordered structured errors. |
| Advanced | [`quota-ledger`](quota-ledger/README.md) | Model capacity-bounded reserve/release operations with contracts and property tests. |

Most of the more involved examples use fixed fixtures and in-memory
calculations to cover practical workflows without external services or
persistent state. `quota-ledger` is a pure library with generated property
cases instead of a command-line workflow. Each project README lists the
language features it illustrates and the compiler stages that are still
needed to build and run it.
