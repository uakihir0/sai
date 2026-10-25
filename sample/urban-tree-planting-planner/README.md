# Urban Tree Planting Planner

Urban Tree Planting Planner builds a small planting plan for a fictional
neighborhood. It assigns named tree species to priority-ranked sites only when
drainage, root volume, overhead clearance, and sidewalk width all meet the
fixture's requirements. A shared nursery inventory, district diversity limit,
and establishment-watering budget constrain the plan.

The fixture is hand-calculated from the rules in `planner.sai`; the report is
not compiler output:

```text
Urban tree planting plan — North Valley, spring round
Order: higher-priority sites first; ties use ascending site ID

#3101 Canal Walk: plant Swamp white oak; reserve 72 L for establishment watering
#3102 Civic Steps: plant Serviceberry; reserve 28 L for establishment watering
#3103 Depot Island: held — no species fits soil and site clearances
#3104 Library Court: plant Ironwood; reserve 52 L for establishment watering
#3105 Canal Pocket: plant Swamp white oak; reserve 72 L for establishment watering
#3106 Maple Row: plant Kentucky coffeetree; reserve 44 L for establishment watering
#3107 South Wharf: held — district species limit reached
#3108 North Park: held — shared watering budget is insufficient
#3109 East Market: plant Serviceberry; reserve 28 L for establishment watering
#3110 North Annex: held — no compatible nursery stock remains

Planted: 6/10; held: 4
Establishment watering: 296/300 L reserved; 4 L remain
```

## Planning flow

1. `model.sai` defines site requirements, species constraints, decisions, and
   explicit hold reasons.
2. `data.sai` provides ten planting sites across two districts, four nursery
   species, a district species cap of two, and a 300-liter city watering
   budget. The input sites are deliberately not in scheduling order.
3. `planner.sai` orders sites by ascending numeric priority and then site ID.
   It rejects species that do not fit a site's drainage or physical clearances,
   are out of stock, exceed the district cap, or need more than the remaining
   water budget. It reserves inventory and water only after choosing a tree.
   When several constraints block different otherwise-compatible species, the
   hold reason names the furthest eligibility stage reached: site fit, nursery
   stock, district diversity, then water budget.
4. When several species remain feasible, the planner picks the one least
   represented in that district, then the one with lower establishment water,
   then the lower species ID.
5. `report.sai` renders a readable planting plan with species names, reserved
   watering, and an explanation for each held site. `lib.sai` exposes the plan
   and report and includes source-level regression examples.

## Limits

This is a deterministic, fixed-input planning example. It does not map actual
soil, utilities, roots, mature canopy, pests, climate, nursery deliveries,
watering infrastructure, or maintenance crews. Every fit check assumes the
fixture's measurements are correct. A selected tree is a candidate for
professional review, not a recommendation to plant that species at a real
site. The planner also expects site and species IDs to be unique; it does not
validate arbitrary caller-provided identifiers.

## Review required

- The exact drainage match and minimum root-volume, overhead-clearance, and
  sidewalk-width thresholds are invented teaching rules. An arborist and local
  utility and public-works authorities must validate species suitability and
  site measurements before real planting.
- The district limit of two per species promotes a simple form of diversity;
  it does not measure genetic diversity, pest resilience, native status, or
  canopy equity. A city ecologist should define an appropriate planting
  strategy.
- Establishment-watering amounts are fixed per tree and reserve against one
  citywide budget. The model does not schedule watering over time, account for
  rainfall or drought, or compare demand across districts.
- The priority-ordered greedy choice can use scarce inventory or water on an
  early site and hold later sites. It does not search for a globally optimal
  assignment or minimize total cost.

## Package and compiler status

This is a dependency-free `wasm32-core` library with `alloc` used to build its
human-readable report. `sample_report()` returns the plan as text.
`sai fmt --check sample/urban-tree-planting-planner` passes. The command
`sai check --offline --tests -C sample/urban-tree-planting-planner` stops at
the unimplemented Stage 0 checker and `@derive` feature gates, so semantic
checking, building, test execution, and runtime output are not verified. The
report above is manually reconciled with the fixture.
