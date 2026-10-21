# SAI samples

These small projects illustrate SAI package structure and progressively richer
language features. Some use compiler stages that are still under development,
so they are examples of the intended source code rather than a promise that
every project builds today.

The command-line samples declare the canonical `sai:process` source and `^0.1`
range from the package examples. The `sai:process` Git URL is currently
unavailable, so the dependency cannot currently be fetched from that source.
The package is specified as a toolchain-bundled standard package, so these
manifests need a matching bundled package index or an available published
source repository before a package resolver can fetch them.

- [`hello-world`](hello-world/README.md) writes a fixed greeting to stdout.
- [`greet-cli`](greet-cli/README.md) reads a name from the command-line
  arguments and formats a greeting.
- [`mini-task-board`](mini-task-board/README.md) splits a small task board
  across modules and demonstrates records, enums, command dispatch, and a
  summary.
- [`profile-check`](profile-check/README.md) parses a small profile format,
  validates its fields, and reports recoverable errors.
- [`reading-log`](reading-log/README.md) tracks a fixed reading queue and
  reports progress, status counts, and the next book.
- [`market-checkout`](market-checkout/README.md) models a small market's
  catalog, orders, stock checks, discounts, tax, and operator reports across
  several modules.
- [`event-planner`](event-planner/README.md) tracks event sessions, attendee
  registrations, capacity-based waitlists, open seats, and confirmed schedule
  conflicts across several modules.
- [`turn-based-duel`](turn-based-duel/README.md) narrates a deterministic
  turn-based match and demonstrates enum-driven actions, mutable state,
  stamina rules, a simple opponent strategy, and early termination.
- [`incident-response-console`](incident-response-console/README.md) scores
  fixed operational incidents, calculates response SLAs, and identifies
  pending-response escalations across several modules.
