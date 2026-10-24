# Clockwork Escape Room

Clockwork Escape Room replays a short museum adventure. A visitor finds a key,
opens an archive, repairs an observatory mechanism, aligns a telescope, and
follows its beam into the courtyard. Each choice adds a sentence to a readable
story log.

The project uses a fixed seven-action script rather than interactive keyboard
input. The fixture reaches the courtyard with no rejected actions. Trying to
enter the archive before taking the key leaves the visitor in the workshop and
records one rejected action.

The successful story log includes lines such as:

```text
A brass key rests beside a stopped automaton. A locked door leads to the archive.
You pocket the brass key. The automaton gives a tiny approving nod.
The key turns. Dusty shelves and a clockwork gear fill the archive.
The gear catches with a satisfying click, and the observatory dome opens.
You align the lens. Sunlight draws a bright arrow toward the exit.
The exit clicks open. You step into the courtyard as the automaton waves.
```

## Project structure and concepts

- `model.sai` defines rooms, actions, endings, the adventure state, and its
  bounded scene log.
- `game.sai` applies one action at a time and runs the scripted fixture.
- `lib.sai` exposes `replay_museum_visit()`.

The example illustrates enum-based state machines, pattern matching,
guarded transitions, bounded logs, and a small set of meaningful invariants.
The scene strings make each state change visible to a reader.

## Review required

- The order of puzzles, the gear-and-lens mechanism, and the successful ending
  are fictional teaching choices. They do not model accessibility, alternate
  solutions, visitor safety, or a real museum's security procedures.
- The action list is scripted in source. The project does not read user input,
  print to a terminal, or provide a save file.
- The scene log stores at most eight entries. The supplied fixture uses seven;
  additional actions beyond the capacity still update state but are not added
  to the log.

## Package and compiler status

This is a pure `wasm32-core` library with no I/O, external dependencies, or
persistent state. The source includes tests for the successful story and a
blocked transition. The current compiler rejects `sai check` at its
unimplemented Stage 0 feature gate and `sai test` at its unimplemented Stage 3
feature gate; the intended outcomes are documented here, not compiler output.
