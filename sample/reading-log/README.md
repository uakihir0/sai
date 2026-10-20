# Reading Log

Reading Log keeps a tiny book queue in memory and turns it into a useful
reading report:

```text
reading-log list
Reading queue
[reading] A Wizard of Earthsea — 168/224 pages (75%)
[to read] The Left Hand of Darkness — Ursula K. Le Guin
[finished] Kindred — Octavia E. Butler
[reading] The Dispossessed — 120/387 pages (31%)

reading-log progress
You have read 552 of 1,275 pages (43%).
2 books in progress, 1 finished, and 1 waiting to be read.

reading-log next
Next up: The Left Hand of Darkness by Ursula K. Le Guin.
```

The project separates its book model and sample collection from progress
calculations and terminal reports. It demonstrates records, enums, arrays,
loops, module imports, command dispatch, and reporting derived values. The
queue is fixed sample data: the example does not save changes to disk.

## Package and compiler status

The package targets `wasm32-wasi` and uses the same `sai:process` package and
entry-point shape as the other command-line samples. The command dispatcher
assumes the host supplies the program name as `argv[0]`. This is an
illustrative project rather than a build guarantee. The current compiler does
not yet support the complete package, process, command-line, loop, formatting,
and multi-module pipeline used here.
