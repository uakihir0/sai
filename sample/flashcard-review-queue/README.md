# Flashcard Review Queue

Flashcard Review Queue shows a small spaced-review loop: find cards due today,
apply one recall rating, and calculate the next review day. The four cards are
fixed fixtures, so the example is predictable and easy to change while reading
its source.

```text
flashcard-review-queue due
Due cards (3/4) — day 12
#101 ありがとう — thank you (due day 12)
#102 駅 — station (due day 10)
#103 あさ — morning (due day 12)

flashcard-review-queue preview
Review example: #102 駅 — hard
interval: 7 -> 8 days; next review: day 20; streak 4
```

## Module map

- `model.sai` defines cards and the four recall ratings.
- `data.sai` holds the four-card fixture and the current day.
- `review.sai` selects due cards and applies a rating to produce a new card
  value. `Again` schedules the next day and resets the streak; the other three
  ratings lengthen the interval and streak.
- `report.sai` prints due cards and one `Hard` review preview.
- `lib.sai` dispatches the `due` and `preview` commands.

This sample complements `reading-log` with an active review cycle: recall ratings determine each card’s next due date.

The interval rules are intentionally small teaching heuristics: `Again` uses
one day; `Hard` adds one day (with a two-day minimum); `Good` doubles the
interval (with a one-day minimum); and `Easy` triples it (with a three-day
minimum). A successful rating increments the streak. Applying a review returns
a new value; it does not write a file or modify a persistent deck.

## Package and compiler status

The package targets `wasm32-wasi` and uses the same `sai:process` command-line
entry-point shape as the other CLI samples. The declared `sai:process` Git URL
is currently unavailable, so package resolution still needs a matching
bundled package index or a published source repository. This example also
needs the compiler's package, WASI/process, multi-module, loop, string-format,
and runtime stages before its commands can be built and run. The test
declarations and hand-calculated output document intended behavior; no
successful SAI semantic check or execution is claimed.

## Review required

The interval multipliers, streak behavior, and Japanese vocabulary are
illustrative fixture choices, not learning-science recommendations or a
complete flashcard policy. Review those choices before using the sample to
teach a real course or to schedule a learner's study plan.
