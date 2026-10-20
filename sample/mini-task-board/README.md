# Mini Task Board

A small command-line example with an in-memory task list. It has two commands:

```text
mini-task-board list
#1 [todo] Review the parser recovery notes
#2 [doing] Add a borrow-checker regression
#3 [done] Document the package layout

mini-task-board summary
3 tasks: 1 todo, 1 doing, 1 done
```

The root module handles command dispatch and stdout; `board.sai` owns the task
model, sample data, counting, and output formatting. The project
demonstrates modules, records, enums, arrays, loops, and f-strings. The data is
intentionally in memory, and the WASI entry point uses compiler features that
are still in progress. The command dispatcher assumes the host supplies the
program name as `argv[0]`.
