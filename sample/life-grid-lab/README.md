# Life Grid Lab

Life Grid Lab evolves a fixed 5×5 Conway's Game of Life board. Each new
generation is computed from the complete previous board using the familiar
birth-at-three and survive-at-two-or-three rules.

The horizontal blinker fixture evolves as follows:

```text
.....
.....
.###.
.....
.....

  ↓ one generation

.....
..#..
..#..
..#..
.....
```

After two generations it returns to its horizontal orientation. A 2×2 block
stays unchanged, while an isolated live cell dies. The edge fixture places
live cells at the upper-left, upper-right, and lower-left corners. They all
die, and no cell appears at the lower-right corner. A toroidal grid would give
that lower-right cell three neighbors and make it live.

## Project structure and concepts

- `src/model.sai` defines the fixed board and the generation limit.
- `src/fixtures.sai` provides the blinker, stable block, isolated cell, and
  corner cases.
- `src/simulation.sai` counts each cell's bounded neighborhood, computes the
  next board, and rejects requests above the generation limit.
- `src/lib.sai` exposes the stepping API and embeds assertions for the example
  patterns and limits.

## Review required

- The board is always 5×5, uses row-major Boolean cells, and treats every
  out-of-bounds location as dead. Confirm this finite boundary is appropriate
  for the intended lesson; toroidal wrapping and resizable boards are not
  modeled.
- `advance` accepts zero through eight generations and rejects larger values
  before performing work. This fixed limit keeps runtime work bounded; it is a
  sample policy, not a Conway rule.
- Each cell in a generation is derived only from the previous board. The
  implementation has no random initialization, rendering, I/O, cycle
  detection, or history storage.
- The fixtures and expected patterns are hand checked. They are intended to
  teach Conway's rules, not to establish performance for larger grids.

## Package and compiler status

This is a dependency-free `wasm32-core` library. Run
`sai fmt --check sample/life-grid-lab` from the repository root to check its
source formatting. The embedded assertions document intended behavior, but
the current compiler's Stage 0 feature gate and Stage 3 test-runner gate
prevent semantic type checking and execution of these tests.
