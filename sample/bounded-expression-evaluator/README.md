# Bounded Expression Evaluator

Bounded Expression Evaluator evaluates an already-tokenized expression with
unsigned 32-bit integers. It uses two fixed-capacity stacks to apply
parentheses, precedence, and left associativity without allocating memory.

| Token sequence | Result |
|---|---:|
| `2 + 3 * (4 - 1)` | 11 |
| `8 - 3 - 2` | 3 |
| `9 / 0` | Division by zero at offset 2 |
| `4294967295 + 1` | Arithmetic range error at offset 11 |

Source positions are zero-based byte offsets from the supplied token stream.
`EvalError.position_kind` distinguishes source byte offsets from token-array
indices; token indices identify a missing declared slot or the first
over-capacity token. Errors with no offending source token use `Unavailable`.
The expression text in this table is explanatory: this project accepts `Token`
values and does not include a character lexer or a source-string parser.

## Evaluation flow

1. `fixtures.sai` supplies fixed token streams with source offsets.
2. `evaluator.sai` alternates between expecting an operand and an operator.
3. Number tokens go onto the value stack. Operators wait on the operator
   stack until a lower-precedence operator or a closing parenthesis requires
   reduction.
4. Each reduction checks addition, subtraction, and multiplication before
   arithmetic; division by zero returns a structured error.
5. Errors report a typed position when a source token or token index is
   available.

The sample tests nested precedence, left-associative subtraction, division by
zero, and addition overflow.

## Project structure and concepts

- `model.sai` defines tokens, fixed-size input, evaluation results, and errors.
- `fixtures.sai` contains four hand-authored token streams.
- `evaluator.sai` implements the bounded two-stack evaluator.
- `tests.sai` covers successful evaluation and arithmetic failures.
- `lib.sai` exposes the fixture and token-stream APIs.

## Review required

- This evaluator intentionally supports only nonnegative `U32` literals and
  binary `+`, `-`, `*`, and `/`. Unary operators, remainder, exponentiation,
  decimal values, and implicit multiplication are unsupported.
- Subtraction that would produce a negative value returns
  `ArithmeticRangeError`; the sample does not use signed integers.
- A token stream has a fixed capacity of 32. It assumes token kinds, integer
  values, and byte offsets came from a trustworthy tokenizer; source spelling,
  whitespace, and whether offsets point to UTF-8 boundaries are not checked.
- Arithmetic failures are returned as errors rather than relying on the
  language runtime's overflow or division traps.

## Package and compiler status

This is a pure `wasm32-core` library with no I/O, external dependencies, or
persistent state. The source includes tests for the documented results. The
current compiler rejects `sai check` at its unimplemented Stage 0 feature gate
and `sai test` at its unimplemented Stage 3 feature gate; the table is
hand-checked against the fixtures, not compiler-generated output.
