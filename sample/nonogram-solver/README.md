# Nonogram Solver

Nonogram Solver fills a fixed 5-by-5 grid from row and column run clues. It
enumerates the legal bit patterns for each line, then searches rows in a
stable order. After each row is placed, candidate column patterns prune
partial boards that can no longer satisfy their clues.

The included fixture describes a small X-shaped puzzle. A successful solve
returns one valid board and a summary of its filled cells and clue checks.
When a puzzle has multiple solutions, the solver returns the first one found;
it does not claim that the solution is unique.

`model.sai` defines the puzzle, clues, board, and errors. `fixtures.sai` stores
the fixed puzzle. `patterns.sai` validates clues and enumerates matching line
patterns. `solver.sai` applies column-prefix pruning and deterministic
backtracking. `report.sai` checks the completed board and counts filled cells;
`lib.sai` exposes the API and examples.

## Package and compiler status

This is a pure `wasm32-core` library with no input parsing, terminal output,
external services, or persistent state. Its dimensions are fixed at 5 by 5,
each clue can contain at most three runs, and each line has at most 32
candidate bit patterns. Candidate masks are considered in ascending numeric
order, with the leftmost cell stored in the lowest bit. Search stops at the
first solution. It does not count solutions, prove uniqueness, or optimize
which valid board it returns.

This project illustrates fixed-capacity data, clue validation, candidate
enumeration, constraint pruning, recursive search, and independent result
verification. It is an illustrative package rather than a build guarantee:
the current compiler has not completed the semantic and package pipeline
needed to build this source.
