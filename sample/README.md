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
| Beginner | [`mini-task-board`](mini-task-board/README.md) | Split a small task board into modules; use records, enums, command dispatch, and summaries. |
| Beginner | [`profile-check`](profile-check/README.md) | Parse and validate a key-value profile; return structured errors with `Result`. |
| Intermediate | [`reading-log`](reading-log/README.md) | Summarize a fixed book queue with status counts, page totals, and next-book selection. |
| Intermediate | [`market-checkout`](market-checkout/README.md) | Review orders, aggregate repeated stock requests, and calculate discounts and tax. |
| Advanced | [`event-planner`](event-planner/README.md) | Apply session capacity and waitlists, then detect conflicts between confirmed sessions. |
| Advanced | [`turn-based-duel`](turn-based-duel/README.md) | Simulate stateful turns with combat rules, mutable state, and a deterministic opponent strategy. |
| Advanced | [`incident-response-console`](incident-response-console/README.md) | Score incidents, apply response deadlines, and identify SLA breaches and escalations. |
| Advanced | [`island-grid-dispatch`](island-grid-dispatch/README.md) | Allocate a constrained one-hour power budget by criticality while preserving a battery reserve. |
| Advanced | [`clinic-shift-roster`](clinic-shift-roster/README.md) | Validate clinician credentials, availability, shift caps, rest periods, and required coverage. |

The more involved examples use fixed fixtures and in-memory calculations to
cover practical workflows without external services or persistent state.
Each project README lists the language features it illustrates and the
compiler stages that are still needed to build and run it.
