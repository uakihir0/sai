# SAI samples

These small projects illustrate SAI package structure and progressively richer
language features. Some use compiler stages that are still under development,
so they are examples of the intended source code rather than a promise that
every project builds today.

- [`hello-world`](hello-world/README.md) writes a fixed greeting to stdout.
- [`greet-cli`](greet-cli/README.md) reads a name from the command-line
  arguments and formats a greeting.
- [`profile-check`](profile-check/README.md) parses a small profile format,
  validates its fields, and reports recoverable errors.
- [`mini-task-board`](mini-task-board/README.md) splits a small task board
  across modules and demonstrates records, enums, command dispatch, and a
  summary.
- [`reading-log`](reading-log/README.md) tracks a fixed reading queue and
  reports progress, status counts, and the next book.
- [`market-checkout`](market-checkout/README.md) models a small market's
  catalog, orders, stock checks, discounts, tax, and operator reports across
  several modules.
