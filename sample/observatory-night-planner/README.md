# Observatory Night Planner

This fictional observatory turns eight observing requests into one deterministic
night plan. Each request refers to a catalog target and asks for a filter,
exposure length, observing window, cloud limit, and optional guiding. The
planner ranks requests, checks equipment and conditions, intersects the
request, target, and dark-time windows, then reserves one telescope's time.

The program returns typed decisions and summary counts. The following is a
hand-checked human-readable rendering of those fixture results; it is not
output from an executed SAI program.

```text
North Dome observing log — 18 October
Dark window: 20:10–23:40
Conditions: broken cloud, 18%; no wind hold

20:10–20:30  Comet Vesper-7       Luminance   scheduled
20:36–21:16  Andromeda Galaxy     Luminance   scheduled
21:40–22:10  Jupiter              Planetary   scheduled
23:00–23:25  Crab Nebula          H-alpha     scheduled

Backlog
Helix Galaxy       Held — infrared filter is not installed
Rosette Nebula     Held — guiding unit is unavailable
Pleiades           Held — 18% cloud exceeds its 10% limit
M13                Missed — earliest slot ends at 22:52, after its 21:55 limit

Summary: 4 observations scheduled; 2 equipment holds; 1 cloud hold;
         1 visibility-window miss
```

## Planning rules

- Lower numeric priority is more urgent. Ties use ascending request ID.
- The telescope must support the requested filter. A request requiring guiding
  is held when the observatory has no guiding unit.
- The current cloud percentage must be at or below the request's limit.
- A valid slot must fit inside the intersection of the dark period, the
  request's time window, and the target's visibility window.
- Exposures use the earliest available slot. After the first exposure, the
  planner reserves six minutes for a same-filter slew and twelve minutes for a
  slew plus filter change. A held or missed request consumes no telescope time
  and does not change the filter state.
- Requests are considered once in priority order. The planner does not move an
  earlier exposure to rescue a lower-priority request.

## Modules

- `src/model.sai` defines targets, equipment, sky conditions, requests,
  decisions, and summary records.
- `src/data.sai` provides the eight target catalog entries, telescope,
  observing conditions, and deliberately unsorted request list.
- `src/catalog.sai` resolves each request's target ID.
- `src/policy.sai` checks filter support, guiding, and cloud limits.
- `src/planner.sai` sorts requests and reserves visibility-compatible slots.
- `src/report.sai` counts scheduled exposures and each hold or miss reason.
- `src/lib.sai` exposes `north_dome_report()` and includes source-level
  regression examples.

This is a fixed, in-memory library. It does not read a live ephemeris, query
weather, control a telescope, or guarantee observation quality.

## Review required

- **Review required — time model:** the fixture treats minutes after 20:10 as
  the entire night clock and supplies target visibility windows directly. It
  does not calculate altitude, twilight, or an object's actual position.
- **Review required — setup time:** six-minute slew and twelve-minute
  slew-plus-filter-change allowances are teaching constants, not measured
  observatory timings.
- **Review required — cloud policy:** requests carry their own cloud ceilings,
  and the planner reports only the first applicable equipment or cloud reason.
  Real observing decisions need reviewed safety and scientific-quality rules.
- **Review required — scheduling objective:** a priority-ordered greedy pass
  can miss a request that might fit after moving an earlier observation. The
  fixture demonstrates that tradeoff instead of claiming a globally optimal
  schedule.

## Compiler status

The sample targets `wasm32-core` and has no external packages. Its expected
plan is hand-checked. The current compiler may parse and format this SAI source
without supporting the complete semantic checker or embedded test runner, so
type checking and execution are not claimed.
