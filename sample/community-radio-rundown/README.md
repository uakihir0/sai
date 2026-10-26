# Community Radio Rundown

Community Radio Rundown plans a fictional 30-minute episode against a fixed
clock. It holds a two-minute sponsor break at minute 12, keeps every required
segment, reserves one minute for sign-off, and skips an optional segment if it
would push the closing past the slot.

```text
community-radio-rundown rundown
30-minute community radio rundown
minute 0-1: segment — Opening
minute 1-6: segment — Five-minute headlines
minute 6-12: segment — Repair cafe guest interview
minute 12-14: break — Sponsor break
minute 14-19: segment — Listener mail: fixing an old radio
minute 19-22: segment — Community calendar
minute 22-25: segment — Local band spotlight
minute 25-29: segment — More listener questions
minute 29-30: segment — Closing
1 optional segment held for another episode
ends at minute 30; the 30-minute timing checks pass
```

## Module map

- `model.sai` defines editorial segment kinds and the planned timeline.
- `clock.sai` centralizes the slot length, protected break, and closing
  reservation.
- `data.sai` supplies a ten-item editorial fixture with one optional segment.
- `planner.sai` lays segments out in producer order, pins the break to its
  protected minute, and skips only optional content that cannot fit before
  sign-off.
- `audit.sai` checks the opening and closing positions, the single break,
  slot bounds, overlaps, duplicate IDs, and inclusion of every required item.
- `report.sai` prints the proposed clock and whether the timing checks pass;
  `lib.sai` provides the `rundown` command.

This sample adds a serial broadcast clock to the catalog, with content
placed around a protected break and a hard sign-off.

The fixture deliberately fills the complete clock when the two-minute
behind-the-scenes segment is held: required content ends at minute 29, leaving
one minute for the closing. The planner reports the selected timeline; it does
not play audio, contact a station, or save a broadcast schedule.

## Package and compiler status

The package targets `wasm32-wasi` and uses the same `sai:process` command-line
entry point as the other CLI samples. The declared `sai:process` Git URL is
currently unavailable, so resolution still needs a matching bundled package
index or a published source repository. Building and running the example also
requires package, WASI/process, loop, multi-module, string-formatting, and
runtime support that is not yet complete in the current compiler. The expected
clock above is hand-calculated; no successful SAI semantic check or execution
is claimed.

## Review required

The 30-minute slot, minute-12 break, required-segment policy, and decision to
hold the optional studio segment are teaching fixtures. A real station may
have different sponsor obligations, emergency interrupt rules, music rights,
and producer priorities. Review those assumptions before adapting this
example to an actual broadcast workflow.
