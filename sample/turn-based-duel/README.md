# Turn-Based Duel

Turn-Based Duel is a deterministic, five-round sparring match between Asha
and a clockwork guardian. It narrates each move, shows how health and stamina
change, and ends when one fighter is knocked out.

```text
turn-based-duel demo
Asha enters the practice ring. The clockwork guardian lowers its shield and squares up.

Round 1
Asha strikes Clockwork Guardian for 3 damage.
Clockwork Guardian recovers 1 health.
  Asha: 10/10 HP, 2/4 stamina
  Clockwork Guardian: 4/6 HP, 4/4 stamina

Round 2
Asha braces and prepares to block, restoring 1 stamina.
Clockwork Guardian strikes Asha, but her guard reduces the hit to 1 damage.
  Asha: 9/10 HP, 3/4 stamina
  Clockwork Guardian: 4/6 HP, 2/4 stamina

Round 3
Asha strikes Clockwork Guardian for 3 damage.
Clockwork Guardian recovers 1 health and 2 stamina.
  Asha: 9/10 HP, 1/4 stamina
  Clockwork Guardian: 2/6 HP, 4/4 stamina

Round 4
Asha recovers 1 health and 2 stamina.
Clockwork Guardian recovers 1 health.
  Asha: 10/10 HP, 3/4 stamina
  Clockwork Guardian: 3/6 HP, 4/4 stamina

Round 5
Asha strikes Clockwork Guardian for 3 damage.
  Asha: 10/10 HP, 1/4 stamina
  Clockwork Guardian: 0/6 HP, 4/4 stamina

Asha wins the sparring match.
```

## Rules

- `Strike` costs 2 stamina and deals 3 damage. If the target is guarding, the
  hit deals 1 damage and consumes that guard. A fighter without enough stamina
  cannot strike.
- `Guard` reduces the next incoming strike to 1 damage and restores 1 stamina,
  up to the fighter's limit.
- `Recover` restores up to 1 health and 2 stamina.
- The clockwork guardian recovers at 3 health or less, or when it has less
  than 2 stamina; otherwise it strikes.
- Asha's five actions and both fighters' starting stats are fixed in the
  example data, so the narrated outcome is reproducible.

`model.sai` declares the actions, fighters, and action reports. `rules.sai`
applies one move, `strategy.sai` selects the guardian's response, and
`duel.sai` runs the turn sequence. `report.sai` turns each state change into
text. The sample demonstrates enums, `match`, mutable borrows, a stateful
loop, a deterministic strategy, and narrative output.

## Package and compiler status

The project targets `wasm32-wasi` and uses `sai:process` for its command-line
entry point. It is illustrative, not a build guarantee: the current compiler
does not yet support the complete package, process/WASI, borrowing, loop,
formatting, and multi-module pipeline used by the example. The host supplies
the executable name as `argv[0]`. The duel uses fixed data; it has no
randomness, file access, persistence, or user-controlled combat input.
