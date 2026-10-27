# Workbook Recalculator

Workbook Recalculator is a tiny spreadsheet engine for a fixed invoice and a
set of deliberately broken formulas. Each cell is either a `U32` literal or a
binary formula that refers to two other cells by ID. The calculator resolves
references, finds dependency cycles, evaluates independent formulas, and
reports why the remaining cells could not be calculated.

The fixture is intentionally stored out of dependency order. It calculates a
small invoice while independently reporting an unknown reference, a two-cell
cycle, unsigned overflow, and division by zero.

## Intended fixture result

| Cell ID | Label | Result |
|---:|---|---|
| 10 | Hours | `8` |
| 11 | Rate | `25` |
| 20 | Gross | `200` (`Hours * Rate`) |
| 40 | Tax percent | `15` |
| 50 | Tax base | `3000` (`Gross * Tax percent`) |
| 60 | Tax | `30` (`Tax base / 100`) |
| 100 | Percent base | `100` |
| 101 | Net | `170` (`Gross - Tax`) |
| 80 | Missing input | `MissingReference` (cell 999 does not exist) |
| 81 | Missing downstream | `BlockedByError`, first blocked by cell 80 |
| 90, 91 | Cycle A, Cycle B | `Cycle` |
| 92 | Cycle downstream | `BlockedByCycle`, first blocked by cell 90 |
| 93 | Overflow | `ArithmeticRangeError` (`U32_MAX + 8`) |
| 95 | Overflow downstream | `BlockedByError`, first blocked by cell 93 |
| 96 | Divide by zero | `DivisionByZero` (`8 / 0`) |
| 97 | Division downstream | `BlockedByError`, first blocked by cell 96 |

The stable formula evaluation order is `20, 50, 93, 96, 60, 101`. Cells 93
and 96 appear in the order because their operands were available and their
formulas were attempted, even though each attempt produced an arithmetic
error. Formulas waiting on a result are skipped until a later pass.

## Recalculation algorithm

1. Validate the declared workbook length, every occupied slot, unique cell
   IDs, and empty slots beyond the declared length.
2. Mark literals as ready. A formula with either unknown reference receives
   `MissingReference` and is excluded from dependency-cycle analysis.
3. Build a fixed 19-by-19 formula dependency matrix and compute its transitive
   closure. A cell that reaches itself belongs to a cycle; a formula that
   reaches a cycle member is instead `BlockedByCycle`.
4. Scan formulas in workbook slot order. Evaluate a formula once both inputs
   are ready. Repeat at most one pass per declared cell, so a chain of
   dependencies resolves without recursion or an unbounded worklist.
5. Preserve each cell's result independently. Arithmetic errors and invalid
   references block only their dependants; unrelated formulas still finish.

The closure distinguishes a member of a cycle from a formula merely waiting
on one. When both inputs fail, the left input takes precedence; a pending left
input keeps the formula pending until that input resolves. For a formula blocked
by multiple cycle members, the first member in workbook slot order is recorded.
These rules keep reports deterministic.

## Project structure

- `src/model.sai` defines cells, formulas, statuses, results, and bounds.
- `src/fixtures.sai` builds the fixed invoice, missing-reference, cycle, and
  arithmetic-error cases.
- `src/lookup.sai` validates cell IDs and the declared workbook extent.
- `src/formula.sai` evaluates one binary operation with checked `U32`
  arithmetic.
- `src/formula/graph.sai` builds dependency edges and their bounded
  transitive closure.
- `src/recalculate.sai` classifies cycles and runs deterministic passes.
- `src/lib.sai` exposes the report and source-level regression assertions.

## Review required

- Cell IDs are plain `U32` values rather than spreadsheet coordinates such as
  `A1`. The example focuses on dependency semantics and does not parse cell
  addresses or formula text.
- Cycle membership is computed with transitive closure, then non-cycle
  dependants receive `BlockedByCycle`. The recorded blocker is the first
  reachable cycle member by workbook slot order, not a complete cycle path.
- Formula evaluation follows workbook slot order on every pass. A formula is
  listed when it is attempted with two ready operands, including formulas that
  fail arithmetic; blocked and cycle cells are not listed as evaluated.
- Addition and multiplication report range errors instead of wrapping or
  saturating. Subtracting a larger unsigned value also reports a range error.
  Integer division truncates toward zero; the tax fixture avoids a fractional
  result, but this arithmetic policy should be revisited before using the
  example for money.
- The workbook capacity is 19 cells and each formula has exactly two inputs.
  There are no blank cells within the declared range, text values, ranges,
  recalculation edits, floating point, or user-entered formulas.
- Fixture results and formula order are hand-checked. The source assertions
  describe intended behavior, but the current compiler pipeline has not
  executed them.

## Formatting and compiler status

Run `sai fmt --check sample/workbook-recalculator` from the repository root to
check source formatting. The table above is hand-checked fixture output, not
compiler output. The current compiler stops semantic checking at its documented
Stage 0 and does not yet run source tests, so this project is not claimed to
build or execute.
