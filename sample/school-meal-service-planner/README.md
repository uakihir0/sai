# School Meal Service Planner

This fictional district kitchen prepares one lunch service for 96 students and
four reserve portions. The planner compares seven complete menu options against
a frozen day-18 ingredient snapshot, the school's allergy roster, per-portion
nutrition bands, a single-kitchen production window, and a daily ingredient
budget. It reports one stable first rejection reason for each infeasible menu,
then selects the cheapest menu that passes every check.

## Fixture

- The service needs 100 portions: 96 enrolled students plus 4 reserve portions.
- The school roster excludes peanuts and sesame. Dairy and wheat are not
  excluded in this fixture. Ingredient allergen flags come from the fixture;
  they are not an ingredient-label lookup service.
- Each portion must contain 500–700 kcal and 18–32 grams of protein, inclusive.
  Nutrition values are supplied estimates, not derived from ingredient data.
- The kitchen can prepare at most 60 portions per batch and has 90 total prep
  minutes. Batch count is the ceiling of portions divided by 60; total prep
  minutes are batch count multiplied by each recipe's minutes per batch.
- Ingredient inventory is usable on day 18 when `use_by_day >= 18`. The corn
  fixture has 6,000 usable grams and 5,000 grams from a day-16 lot that is
  expired for this service. The corn bake requires 9,000 grams, so the expired
  quantity cannot fill its 3,000-gram shortfall.
- The ingredient budget is 7,800 cents ($78.00). Projected recipe cost is
  calculated from whole-portion ingredient grams and fixture cents per 100 g,
  rounded down to whole cents for each ingredient line. It excludes labor,
  utilities, tax, packaging, and food waste.
- Every option is checked independently against the same inventory snapshot.
  Only the single selected menu is intended to be prepared.

The fixed menus are tomato barley stew, chickpea rice bowl, peanut noodle bowl,
lentil potato soup, corn and white bean bake, salmon rice bowl, and baked chicken
pasta. Tomato barley stew and chickpea rice bowl pass all rules. The stew has a
projected batch cost of $39.80 and the rice bowl costs $47.40, so the stew is
selected. Rejected fixtures demonstrate distinct constraints: peanut exclusion,
calories below the minimum, expired-lot inventory shortfall, budget excess, and
kitchen prep time above the available shift.

## Intended commands and output

- `school-meal-service-planner plan` prints constraints, every candidate's
  first blocking rule, and the selected menu.
- `school-meal-service-planner options` prints the candidate decisions without
  the final selection paragraph.
- `school-meal-service-planner summary` prints eligible/rejected counts,
  portions, the budget, and the selected option.

The hand-checked plan is expected to select **Tomato barley stew** for 100
portions: two 60-portion-capacity batches, 50 of 90 prep minutes, and $39.80 of
projected food cost. The other accepted option is Chickpea rice bowl at $47.40.

The expected first-reason summary is:

| Menu | Expected decision | Deterministic explanation |
|---|---|---|
| Tomato barley stew | Eligible | $39.80 projected ingredient cost; 2 batches and 50 prep minutes |
| Chickpea rice bowl | Eligible | $47.40 projected ingredient cost; 2 batches and 56 prep minutes |
| Peanut noodle bowl | Rejected | Peanut sauce contains an excluded peanut allergen |
| Lentil potato soup | Rejected | 480 kcal is below the 500 kcal minimum |
| Corn and white bean bake | Rejected | Needs 9,000 g of corn; 6,000 g is usable and 5,000 g is expired |
| Salmon rice bowl | Rejected | $116.00 projected food cost exceeds the $78.00 budget |
| Baked chicken pasta | Rejected | 2 batches need 100 prep minutes; the kitchen allows 90 |

The planner reports the first blocking rule in this fixed order: allergy
conflict, nutrition band (calories low, calories high, protein low, protein
high), kitchen prep capacity, usable ingredient inventory, then budget. Within
an allergy check, recipe lines are visited in order and allergens are checked
as peanut, sesame, dairy, then wheat. Inventory shortages are reported for the
first short recipe line. Passing menus are ranked by lower total food cost,
then fewer prep minutes, then ascending menu ID. This makes explanations and
selection independent of hash iteration or unspecified tie behavior.

## Modules and concepts

- `model.sai` defines ingredients, dated inventory lots, recipe lines, menu
  options, constraints, rejection codes, and evaluation records.
- `data.sai` holds the school service, kitchen capacity, seven menus, ingredient
  metadata, and inventory lots.
- `inventory.sai` aggregates usable and expired lot quantities and finds the
  first ingredient shortfall for a menu.
- `policy.sai` evaluates ingredient allergen flags and nutrition bands.
- `planner.sai` computes batch counts, projected costs, first-failure decisions,
  and stable menu selection.
- `report.sai` formats candidate, plan, and summary views; `lib.sai` dispatches
  the command-line subcommands.

The package exercises records, enums, fixed arrays, module imports, loops,
options and matches, multi-constraint planning, per-lot date filtering,
integer-cent arithmetic, deterministic tie-breaks, and explanations linked to
specific fixture inputs.

## Assumptions, limits, and readiness

This is an illustrative, read-only planner for a fixed fictional fixture. It
assumes positive kitchen batch capacity, unique ingredient and menu IDs, valid
recipe ingredient IDs, zero-gram padding in unused recipe slots, bounded
quantities that fit in `U32`, and complete allergen metadata. The roster-wide
allergy rule is a teaching input and is not medical advice or a guarantee that
a production meal is safe. Nutrition values
and prices are provided fixture estimates; the planner does not validate labels,
calculate nutrition from recipes, or verify prices against suppliers.

The planner does not reserve stock, choose individual lots, optimize across
multiple service days or kitchens, split menu options, substitute ingredients,
model preparation dependencies, account for labor or waste, store student
records, or coordinate concurrent kitchen users. Lots with a use-by day equal
to the service day are treated as usable by explicit fixture policy. Each
candidate is independently evaluated; only one winner is selected after all
candidates are checked.

## Review-required fixture judgments

- **Review required — allergy scope:** one roster-wide peanut/sesame exclusion
  is applied to every menu. This keeps student identities out of the fixture
  and makes a conflict reject the full menu; an operational service would need
  a reviewed, student-aware accommodation and cross-contact process.
- **Review required — date boundary:** a lot is usable when `use_by_day` equals
  the service day. This inclusive boundary is explicit and easy to reproduce,
  but should follow the district's actual food-safety policy.
- **Review required — cost and feasibility order:** only ingredient costs count
  toward the budget, and candidates report one first failure in the documented
  rule order. This makes the example auditable and deterministic; production
  policy may prefer a different budget model or a complete set of blockers.
- **Review required — batch model:** batches are sequential, each holds up to 60
  portions, and each takes the menu's fixed prep minutes. This makes capacity
  arithmetic transparent; real equipment, staffing, cleanup, and parallel
  work would need a richer schedule model.

The sample uses the same illustrative `sai:process` entry point and
multi-module package pattern as other command examples. `sai fmt --check`
passes. `sai check --target wasm32-wasi --tests --offline` parses the package,
but semantic checking stops at the current Stage 0 `sai check`, Stage 3
`sai:process` dependency, and `@derive` feature gates. Its manifest depends on
package resolution (Stage 3), and running the WASI command requires the Stage 4
`run` path. The command output and decisions below are hand-checked
expectations, not captured output from an executed program.
