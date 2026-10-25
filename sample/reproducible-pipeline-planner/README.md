# Reproducible Pipeline Planner

This fixed-fixture library plans a small build and release workflow. It
validates the task dependency graph, marks work with invalid inputs, propagates
failed or invalid prerequisites to downstream tasks, and schedules ready work
in bounded batches under worker-slot and shared-resource limits.

The fixture has 11 tasks, three worker slots, and a resource budget of four
units per batch. Ready work is selected by descending priority and then
ascending task ID. A task that does not fit the remaining resource budget is
skipped while the planner considers lower-ranked ready tasks that might fit.
All tasks in a batch finish before the next batch is planned.

## Hand-checked fixture result

| Batch | Slot | Task ID | Task | Resource units | Result |
|---:|---:|---:|---|---:|---|
| 1 | 1 | 10 | Fetch source | 1 | Succeeded |
| 2 | 1 | 20 | Verify source signatures | 2 | Succeeded |
| 2 | 2 | 30 | Lint source | 1 | Succeeded |
| 2 | 3 | 110 | Record build provenance | 1 | Succeeded |
| 3 | 1 | 40 | Run unit tests | 3 | Failed |
| 3 | 2 | 60 | Check documentation links | 1 | Succeeded |
| 4 | 1 | 100 | Run integration tests | 2 | Succeeded |

The fixture produces seven runs across four batches. At batch 3, the unit-test
task is selected first and consumes three of the four resource units.
Integration tests are ready but need two units, so they do not fit the
remaining budget; the one-unit documentation check is selected instead. After
the unit-test failure, release packaging is blocked. Dependency scanning is
invalid in the fixture, so release signing is blocked, and publishing is
blocked by signing. The independent integration tests still run and succeed.

Final task states are:

| Task ID | State | Blocking prerequisite |
|---:|---|---:|
| 10 | Succeeded | — |
| 20 | Succeeded | — |
| 30 | Succeeded | — |
| 40 | Failed | — |
| 50 | Blocked | 40 |
| 60 | Succeeded | — |
| 70 | Invalid | — |
| 80 | Blocked | 70 |
| 90 | Blocked | 80 |
| 100 | Succeeded | — |
| 110 | Succeeded | — |

For a blocked task, `blocked_by` records the lowest-ID direct prerequisite
already terminal when the block is first propagated. It is an immediate
dependency explanation, not a complete root-cause set.

## Modules

- `src/model.sai` defines fixed table capacities, tasks, dependencies,
  configuration, task states, graph/configuration issues, and report records.
- `src/fixtures.sai` supplies the 11-task pipeline, its 12 dependency edges,
  and a three-slot/four-unit scheduling budget.
- `src/lookup.sai` resolves task IDs to their stable fixture-table indices.
- `src/graph.sai` rejects duplicate task IDs, unknown endpoints, self-edges,
  duplicate edges, and dependency cycles using a stable bounded topological
  check.
- `src/planner.sai` validates limits, propagates invalid/failed/blocked
  prerequisites to a fixed point, selects the highest-priority fitting ready
  task for each available slot, and records each completed batch.
- `src/lib.sai` exposes `fixture_report()` and contains source-level assertions
  for the schedule, resource-aware selection, dependency propagation, cycle
  rejection, and an impossible resource configuration.

The model uses fixed arrays and an in-memory fixture. It performs no network,
filesystem, package build, or deployment operations. Execution outcomes are
preloaded fixture values; the planner does not run shell commands or workers.

## Review required

- Priority descending followed by task ID ascending is a simple deterministic
  tie-break policy. A production pipeline may instead need tenant fairness,
  critical-path urgency, deadlines, starvation prevention, or explicit
  operator ordering.
- Resource units are additive and reset for each batch. The model does not
  distinguish resource kinds, placement constraints, persistent reservations,
  task duration, worker health, or resources released during a batch.
- Every task in a batch is treated as having completed before the next batch
  begins. There are no retries, cancellation, timeouts, partial results,
  dynamic task discovery, or asynchronous completion events.
- Any invalid, failed, or blocked direct prerequisite blocks a pending task.
  The report retains only the first terminal prerequisite found by ascending
  ID. Teams should decide whether independent prerequisites should still run
  and whether explanations need the full transitive cause chain.
- The fixture treats task 70's input as invalid and task 40's run as failed.
  These states demonstrate propagation; they are not conclusions from real
  artifact or test evidence.
- A valid DAG and positive per-task cost no larger than the total budget should
  always make progress when pending work becomes ready. `NoProgress` remains
  a fail-closed defensive outcome for an unexpected inconsistent state.

## Formatting and compiler status

Run `sai fmt --check sample/reproducible-pipeline-planner` to check formatting.
The expected schedule above is hand-checked and is not compiler output. The
embedded tests document intended behavior, but the current compiler does not
yet implement all Stage 0 `sai check`, `@derive`, and Stage 3 `sai test`
requirements, so this project is not claimed to build or execute.
