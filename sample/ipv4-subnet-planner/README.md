# IPv4 Subnet Planner

This bounded planning library assigns service VLAN requests to aligned blocks
inside the fictional `10.64.8.0/24` pool. It reserves each subnet's network
and broadcast addresses, chooses the smallest fitting power-of-two block,
allocates larger blocks first, and reports every decision in the original
request order. The plan also exposes valid requests in allocation order,
including the requests that later exceed pool capacity. The input is
intentionally scrambled so allocation order and stable request-ID tie-breaks
can be inspected separately from the report.

The sample is more demanding than a single queue or timeline: it combines
input validation, duplicate detection across the full batch, stable sorting,
CIDR sizing, aligned first-fit placement, capacity failures, and an independent
invariant audit. IPv4 allocation is not represented elsewhere in the catalog;
the offsets keep the algorithm focused on subnet math rather than string
parsing.

## Hand-checkable fixture

Offsets are the final octet of the address, so offset `0` is `10.64.8.0` and
offset `255` is `10.64.8.255`. Each range below includes network and broadcast
addresses. The allocator sorts valid requests by descending block size, then
ascending request ID. It exposes that complete order, including eligible
requests that do not fit; decisions are returned in the unsorted input order.

| Request | Requested hosts | Result | Address range | Usable hosts |
|---|---:|---|---|---|
| Test lab (1) | 70 | Allocated `/25` | `.0–.127` | `.1–.126` |
| Office devices (2) | 40 | Allocated `/26` | `.128–.191` | `.129–.190` |
| Application servers (10) | 20 | Allocated `/27` | `.192–.223` | `.193–.222` |
| Printers (7) | 10 | Allocated `/28` | `.224–.239` | `.225–.238` |
| Cameras (11) | 5 | Allocated `/29` | `.240–.247` | `.241–.246` |
| Wi-Fi access points (20) | 5 | Allocated `/29` | `.248–.255` | `.249–.254` |
| Badge readers (30) | 5 | Rejected: pool exhausted | — | — |
| Visitor Wi-Fi (40) | 5 | Rejected: pool exhausted | — | — |
| Duplicate network A (50) | 4 | Rejected: duplicate request ID | — | — |
| Duplicate network B (50) | 6 | Rejected: duplicate request ID | — | — |
| Empty request (8) | 0 | Rejected: no hosts requested | — | — |
| Oversized lab (9) | 255 | Rejected: more than 254 hosts | — | — |

The table groups allocations in processing order before rejected inputs for
readability; the returned decision array keeps the fixture's input order. The
plan contains six allocations and six rejections, and its high-water offset is
256, exactly the end of the pool. The duplicate-ID rule rejects every request
with a repeated ID, independent of input order, so neither duplicate can
silently win. The two unplaced valid requests remain visible as capacity
rejections. `lib.sai` asserts the exact block boundaries, prefixes, rejection
reasons, summary counts, repeatable layout properties, and that overlap detection treats
subnet ranges as half-open intervals while the audit rejects a corrupted plan.

## Module map

- `src/model.sai` defines request, allocation, decision, rejection, and plan
  records.
- `src/data.sai` supplies the fixed, intentionally unordered twelve-request
  fixture.
- `src/validation.sai` checks duplicate IDs and host-count limits, computes
  power-of-two block sizes, and maps block sizes to prefixes.
- `src/ordering.sai` uses stable insertion sort, with request ID as the tie
  breaker for equal-sized blocks.
- `src/planner.sai` builds the full decision report and performs aligned
  first-fit allocation without discarding requests that do not fit.
- `src/audit.sai` independently checks request/result correspondence, the
  candidate ordering and capacity decisions, valid ranges and host capacity,
  summary counts, and pairwise non-overlap.
- `src/lib.sai` exposes the sample fixture plan and embeds assertions for the
  documented behavior and a deliberately corrupted overlapping plan.
  `src/audit.sai` also tests overlapping and exactly adjacent ranges directly.

## Package and compiler limits

This is a pure `wasm32-core` library. It has no filesystem, network, process,
external dependency, mutable global state, or real infrastructure side effect.
The fixture and planner are fixed to twelve requests and one `/24` pool. The
input gives host counts and numeric request IDs; the source does not parse IP
text or arbitrary CIDR prefixes. The pool has no gateway, static reservations,
exclusions, multiple VLANs, routing policy, or address-family selection.

The sample describes a proposed allocation only. It does not configure
interfaces, DHCP, DNS, or cloud networking and should not be used to change a
real network. Package metadata and the embedded assertions document the
intended behavior; they do not imply that the current compiler can build or
execute this program. `sai fmt --check` checks parsing and canonical formatting.
The current `sai check` reports `FEATURE_NOT_IMPLEMENTED` for import/use
resolution and type checking, and for the sample's `@derive` attributes. The
Stage 3 test runner is not implemented, so the embedded `test` declarations are
source assertions and have not been run.

## Review required

- Rejecting all entries with a duplicate request ID is a fail-closed policy.
  A production allocator may instead need a globally unique namespace or an
  explicit rule to merge repeated service requests.
- Each child subnet reserves both network and broadcast addresses, including
  `/31` or `/32` edge behavior by refusing to create a block smaller than
  four addresses. This models traditional IPv4 subnets and deliberately does
  not support point-to-point `/31` assignments.
- Largest-block-first packing reduces avoidable fragmentation, but it may not
  reflect service priority, ownership, or fairness requirements. Equal-size
  blocks use ascending numeric request IDs for reproducibility.
- The pool is assumed wholly available, and its capacity excludes no router,
  gateway, infrastructure, or reserved addresses. A reviewed network plan
  would need those exclusions and an authoritative inventory.
- Numeric final-octet offsets are a teaching representation for one `/24`;
  the base network and host counts are fictional. No live network facts were
  queried or validated.
