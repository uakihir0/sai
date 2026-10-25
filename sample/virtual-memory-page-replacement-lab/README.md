# Virtual memory page replacement lab

Compare FIFO, least recently used (LRU), and Clock page replacement against the
same short read/write trace. The simulator records every hit or fault, the
resident frame layout after each access, dirty-page evictions, and the final
flush. This is a bounded in-memory teaching model; it does not read process
memory or perform disk I/O.

## Workload

The lab uses three frames and these twelve page accesses:

```text
W2  R1  W2  R3  R5  W2  R3  W4  R5  R3  W4  R5
```

A write marks a resident page dirty. A later read leaves that dirty bit set. If
a dirty page is evicted, the step records one writeback. At the end, the
fixture flushes every remaining dirty page and records those writes separately.

## Results

The following totals are hand-calculated from the fixture and asserted in
`src/lib.sai`:

| Policy | Hits | Faults | Dirty eviction writes | Final flush writes | Total writes | Final frame pages |
|---|---:|---:|---:|---:|---:|---|
| FIFO | 4 | 8 | 2 | 1 | 3 | `[3, 5, 4]` |
| LRU | 6 | 6 | 1 | 1 | 2 | `[5, 4, 3]` |
| Clock | 5 | 7 | 2 | 1 | 3 | `[5, 3, 4]` |

Frame order is physical slot order. All final frames are clean because the
fixture flushes dirty pages before returning the report.

The eviction choices expose where the policies diverge:

| Access | FIFO | LRU | Clock |
|---|---|---|---|
| `R5` at 5 | evict dirty page 2 | evict clean page 1 | evict dirty page 2 |
| `W2` at 6 | evict clean page 1 | hit | evict clean page 1 |
| `R5` at 9 | hit | evict dirty page 2 | hit |
| `R3` at 10 | evict clean page 5 | hit | evict dirty page 2 |
| `R5` at 12 | evict dirty page 2 | hit | hit |

FIFO chooses the page loaded earliest, even when an intervening access made
that page hot. LRU chooses the least recently accessed resident page, with the
lowest frame index as its tie-break. Clock scans from its hand, clearing each
referenced bit as a second chance; it chooses the first clear bit and advances
the hand after a replacement. Hits set the referenced bit but do not move the
hand. When space is free, all policies use the lowest free frame; Clock advances
its hand past that frame.

## Project layout

- `src/model.sai` defines fixed-capacity input, step, frame, and report types.
- `src/simulator.sai` validates input, runs each policy independently, and
  records access-level outcomes.
- `src/fixtures.sai` defines the read/write workload.
- `src/lib.sai` provides fixture entry points and focused regression tests.

The simulator accepts 1–4 frames and at most 12 accesses. It rejects missing
entries and page ID 0 before producing any partial policy results. This sample
uses no external packages or persistent state. `sai fmt --check` parses and
formats its source. The current toolchain reports `sai check` and `sai test` as
unimplemented (and does not yet implement `@derive`), so the SAI regression
blocks cannot run here. The expected fixture totals were independently checked
with a small reference simulation.

## Review required

- The fixture treats one dirty resident frame as one writeback, regardless of
  page size. This keeps the comparison focused on replacement decisions rather
  than modeling storage bandwidth.
- Clock preserves the per-frame reference bits after a scan and advances its
  hand only after loading into a free frame or replacing a victim. Alternative
  reset/hand conventions can change the trace and should be reviewed before
  changing the expected results.
- The LRU tie-break uses the lowest physical frame index. Timestamps are unique
  for valid sequential accesses, so the tie-break is defensive and deterministic.
- The hostile review found missing final-frame and capacity-bound assertions;
  the fixture regression tests now cover every active final frame and both
  upper bounds. A second review pass found no remaining actionable issues.
