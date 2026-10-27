# Bounded Bytecode Verifier

Bounded Bytecode Verifier checks a tiny typed, operand-stack instruction set
before a virtual machine would execute it. The interesting part is control
flow: each reachable instruction receives an abstract stack, and every
incoming branch must agree on the stack depth and the kind of every value.

The first fixture contains a conditional branch that chooses `42` or `0`:

```text
pc  instruction
 0  PushI32(3)
 1  PushI32(7)
 2  LessI32
 3  JumpIfFalse(6)
 4  PushI32(42)
 5  Jump(7)
 6  PushI32(0)
 7  ReturnI32
```

Its two paths reach instruction 7 with exactly one `I32`. The verifier accepts
it, reports 8 reachable instructions, and records a maximum operand-stack
depth of 2. A second fixture reaches instruction 5 once with an empty stack
and once with `[I32]`; it fails with `InconsistentStackAtJoin` and names both
predecessor instructions.

## How the verifier works

1. `fixtures.sai` builds small bytecode streams in fixed-capacity arrays.
2. `verifier.sai` starts instruction 0 with an empty abstract stack and
   processes reachable instructions in a deterministic FIFO worklist.
3. Each instruction checks its input kinds and updates the abstract stack.
   Conditional branches add both their target and fallthrough edges.
4. The first edge to a program counter stores its stack. Later edges must
   match that shape exactly; each instruction is queued only once, so loops
   terminate without repeated state changes.
5. Return instructions must match the declared return kind and consume the
   entire stack. Reachable paths cannot fall through past the final
   instruction.

## Project structure

- `model.sai` defines the instruction set, abstract stack, worklist, and
  structured verification errors.
- `fixtures.sai` supplies a balanced branch, a consistent infinite loop, two
  mismatched joins (different depths and different kinds at one depth), a
  wrong operand kind, an invalid target, and a missing return.
- `verifier.sai` performs bounded control-flow and operand-stack verification.
- `lib.sai` exposes the accepted fixture and source-level tests.

## Review required

- This is a teaching verifier for a deliberately small instruction set, not a
  security boundary for production bytecode. It checks stack kinds, branch
  targets, join agreement, and return shape. It does not validate instruction
  encodings, local-variable initialization, integer overflow behavior, memory
  access, exception edges, or resource consumption at runtime.
- The program is limited to 24 instructions and the stack to 8 values. Every
  valid branch target must name an instruction in the stream; targeting the
  position immediately after the last instruction is rejected.
- An infinite loop with consistent stack state is accepted even if no return
  is reachable. This verifies type safety of reachable instructions, not
  termination; `consistent_loop` exercises this behavior.
- The worklist visits each instruction at most once because the verifier
  rejects a second, different abstract stack instead of widening it. The
  first conflicting edge is reported in stable target-before-fallthrough FIFO
  order.
- Fixture outcomes and expected error locations are hand-checked. They have
  not been confirmed by compiler execution.

## Package and compiler status

This is a pure `wasm32-core` library with no I/O, external dependencies, or
persistent state. It illustrates typed stack simulation, control-flow graph
edges, a bounded worklist, deterministic join diagnostics, and negative test
fixtures. The current compiler has not completed the semantic and package
pipeline needed to build and run this sample.
