# Wildlife Rescue Dispatch

This fictional county service reviews 13 wildlife rescue calls and assigns each
eligible animal to the nearest clinic that can legally and safely receive it.
`lib.sai` exposes `county_report()`. The following is the hand-checked expected
report; it has not been produced by a successful compiler run.

```text
Wildlife rescue dispatch — Harbor County
Reviewed: 13   Placed: 4   Deferred or held: 9

Rank  Case  Animal              Disposition
1     101   Peregrine falcon    Cedar Wildlife Clinic — 22 min
2     104   Bog turtle          No qualified reptile clinic has a protected-species permit
3     105   Barn owl            Hold — rescue authorization missing
4     109   Great blue heron    No on-call clinic for critical care
5     112   Osprey              No qualified clinic within 60 min
6     102   Mute swan           Marsh Bird Unit — 14 min
7     103   Red fox             Defer — not enough clinician hours
8     106   River otter         Outside county service area
9     107   Box turtle          Ridge Reptile Center — 16 min
10    108   Red-tailed hawk     Cedar Wildlife Clinic — 18 min
11    110   Mink                Defer — clinic intake slots are full
12    111   Gray fox            Reject — invalid treatment-hour estimate
13    113   Salamander          No clinic treats this species

Summary: 4 placed, 2 capacity deferrals, 4 clinic-fit deferrals,
         1 authorization hold, 1 outside area, 1 invalid estimate
```

Critical cases are reviewed first, then urgent, then routine. Ties use the
ascending case ID. Calls outside the service area, calls missing required
rescue authorization, and invalid treatment estimates do not consume clinic
capacity. Protected wildlife needs both confirmed case authorization and a
clinic permit. Critical cases also need an on-call veterinarian. A clinic must
be within 60 minutes and have both an open intake slot and enough treatment
hours. Among clinics that meet all requirements, the nearest available clinic
is selected; ties retain the fixture order.

## Modules

- `model.sai` defines case, clinic, disposition, and report records.
- `fixtures.sai` supplies the fixed Harbor County calls and three clinics.
- `intake.sai` applies the service-area, authorization, and estimate checks.
- `triage.sai` creates a stable urgency and case-ID order.
- `dispatch.sai` selects a clinic while tracking its remaining slots and hours.
- `report.sai` counts placements, holds, deferrals, and rejected records.
- `lib.sai` exposes the report and includes example regression tests.

This is an in-memory library with no file I/O, live dispatch, legal advice, or
external packages. The fixture assumes unique case IDs and accurate clinic
capabilities. Its greedy ordering explains the result but does not optimize
travel across the whole county or reserve real veterinary staff.

## Compiler status

The manifest targets `wasm32-core`. The SAI source and expected counts are
illustrative. The source passes `sai fmt --check`, but `sai check` reports that
the Stage 0 checker and `@derive` are not implemented. Type checking and the
included SAI tests therefore remain unverified; runtime behavior has not been
executed. The report above is hand-checked from the fixed fixtures.
