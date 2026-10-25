# Bounded GC Heap Lab

This fixed-size teaching model traces references from explicit roots, collects
unreachable objects (including a garbage-only cycle), and compacts survivors
into one reusable free extent. It operates on an in-memory description of a
32-word heap; it never inspects or changes the host process's memory.

## Demonstration

Object IDs are stable labels. `start` and `words` describe the object's
pre-collection interval in the simulated heap. Roots 11 and 15 retain objects
11, 13, 15, and 16; objects 13 and 16 form a reachable cycle. Objects 17 and 18
also form a cycle, but neither can be reached from a root.

```text
Before collection: 8 objects, 32 words
Roots: 11, 15
Reachable: 11 -> 13 -> 16 -> 13; 15
Reclaimed: 12 (5 words), 14 (4 words), 17 (3 words), 18 (2 words)

Survivors: 11 @ 0, 13 @ 4, 15 @ 7, 16 @ 9
Reclaimed words: 14
Free words: 19
Free extents before compaction: 3 (largest: 10 words)
Largest free extent after compaction: 19 words
Moved words: 9
```

The four survivors occupy 13 words in original address order. Stable packing
moves each surviving object as a whole, so the movement metric counts 3 + 2 + 4
words for objects 13, 15, and 16. The remaining 19 words are contiguous after
compaction.

## Project layout

- `src/model.sai` defines the fixed object, root, and report capacities.
- `src/fixtures.sai` creates the reachable and unreachable reference cycles.
- `src/collector.sai` validates the layout, performs iterative mark tracing,
  measures the post-sweep gaps, then stably compacts live objects.
- `src/lib.sai` exposes the demo and checks the expected reachability, layout,
  fragmentation, and invalid-input behavior.

The collector rejects zero-sized objects, duplicate IDs, overlapping or
out-of-range blocks, missing references, dangling edges, missing roots, and
capacities larger than their fixed arrays before returning any partial result.
Objects must be supplied in increasing address order. A zero-word heap is
valid only when it has no objects and no roots; an empty object table can also
be collected from a positive-sized heap.

## Review required

- The model assumes every object occupies one contiguous interval and that
  references are explicit object IDs. Real runtimes may have interior pointers,
  object headers, pinning, weak references, finalizers, or concurrent mutation;
  none are modeled here.
- Compaction preserves the original object-table order, which is required to
  match address order. The fixture and tests depend on this stable policy.
- `moved_words` counts the size of each surviving object whose starting address
  changes. It is a deterministic teaching metric, not an estimate of actual
  memory bandwidth.
- Collection failure returns an empty report with `InvalidHeap`; callers do
  not receive a diagnostic identifying the malformed field. A production API
  would likely want structured validation errors.

## Formatting and compiler status

Run the source-format check with:

```sh
sai fmt --check sample/bounded-gc-heap-lab
```

This is a dependency-free `wasm32-core` library. `sai fmt --check` succeeds.
The current compiler rejects `sai check` at its Stage 0 `FEATURE_NOT_IMPLEMENTED`
gate and also reports `@derive` as unsupported. `sai test` is still gated at
Stage 3 with `FEATURE_NOT_IMPLEMENTED`, so semantic type checking and execution
of the embedded assertions have not been verified by the compiler. The
demonstration values are hand-calculated and independently cross-checked with
a small reference script; they are not compiler-generated output.
