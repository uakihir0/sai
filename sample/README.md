# SAI samples

These projects illustrate SAI package structure and progressively richer
language features. The levels below are a reading guide, not a guarantee that
the current compiler can build a sample. The output shown in individual
READMEs describes the intended behavior; it is not a claim that the program
has been built or run.

The command-line samples declare the canonical `sai:process` source and `^0.1`
range from the package examples. The `sai:process` Git URL is currently
unavailable, so the dependency cannot currently be fetched from that source.
The package is specified as a toolchain-bundled standard package, so these
manifests need a matching bundled package index or an available published
source repository before a package resolver can fetch them.

| Level | Sample | Purpose and concepts |
|---|---|---|
| Starter | [`hello-world`](hello-world/README.md) | Print the text greeting `Hello, world!`; introduce a command entry point and stdout. |
| Starter | [`greet-cli`](greet-cli/README.md) | Read a name argument and format a personalized greeting. |
| Starter | [`free-shipping-check`](free-shipping-check/README.md) | Decide whether an order meets a free-shipping threshold with a typed comparison. |
| Beginner | [`temperature-converter`](temperature-converter/README.md) | Convert Celsius to Fahrenheit with `F32` parameters, locals, and arithmetic. |
| Beginner | [`mini-task-board`](mini-task-board/README.md) | Split a small task board into modules; use records, enums, command dispatch, and summaries. |
| Beginner | [`profile-check`](profile-check/README.md) | Parse and validate a key-value profile; return structured errors with `Result`. |
| Intermediate | [`open-source-license-review`](open-source-license-review/README.md) | Review a fixed component inventory for evidence gaps, conflicts, policy exceptions, and unresolved release findings. |
| Intermediate | [`reading-log`](reading-log/README.md) | Summarize a fixed book queue with status counts, page totals, and next-book selection. |
| Intermediate | [`market-checkout`](market-checkout/README.md) | Review orders, aggregate repeated stock requests, and calculate discounts and tax. |
| Intermediate | [`home-energy-bill-auditor`](home-energy-bill-auditor/README.md) | Compare flat and time-of-use tariffs with solar credits, demand charges, bounded integer arithmetic, and tax. |
| Intermediate | [`library-loan-policy`](library-loan-policy/README.md) | Enforce checkout, hold, and renewal rules with records, enums, and `Result`. |
| Intermediate | [`marsh-survey-review`](marsh-survey-review/README.md) | Classify fixed field observations by correction, conservation review, confidence, and transect totals. |
| Intermediate | [`corpus-search-ranker`](corpus-search-ranker/README.md) | Rank a fixed text corpus with whitespace-token matching, weighted title scores, and stable tie-breaking. |
| Advanced | [`event-planner`](event-planner/README.md) | Apply session capacity and waitlists, then detect conflicts between confirmed sessions. |
| Advanced | [`university-enrollment-planner`](university-enrollment-planner/README.md) | Enforce year, program, prerequisite, credit, and timetable rules while ranking eligible section waitlists. |
| Advanced | [`turn-based-duel`](turn-based-duel/README.md) | Simulate stateful turns with combat rules, mutable state, and a deterministic opponent strategy. |
| Advanced | [`incident-response-console`](incident-response-console/README.md) | Score incidents, apply response deadlines, and identify SLA breaches and escalations. |
| Advanced | [`island-grid-dispatch`](island-grid-dispatch/README.md) | Allocate a constrained one-hour power budget by criticality while preserving a battery reserve. |
| Advanced | [`clinic-shift-roster`](clinic-shift-roster/README.md) | Validate clinician credentials, availability, shift caps, rest periods, and required coverage. |
| Advanced | [`court-calendar-planner`](court-calendar-planner/README.md) | Schedule fixed hearings around deadlines, judges, courtrooms, parties, witnesses, and session capacity; report unscheduled cases. |
| Advanced | [`community-food-bank`](community-food-bank/README.md) | Allocate expiring pantry stock across household requests with dietary eligibility and priority rules. |
| Advanced | [`community-irrigation-rotation`](community-irrigation-rotation/README.md) | Plan drought-restricted garden watering across zone windows, field and canal losses, priority order, flow caps, and a weekly allotment. |
| Advanced | [`warehouse-fulfillment`](warehouse-fulfillment/README.md) | Reserve date-eligible inventory for prioritized orders, then choose a warehouse and route under cold-chain, delivery-deadline, and lane-capacity constraints. |
| Advanced | [`rail-shuttle-dispatch`](rail-shuttle-dispatch/README.md) | Assign a fixed replacement-bus fleet to disrupted transit services under corridor, passenger-capacity, and wheelchair-space constraints; report unresolved requests deterministically. |
| Advanced | [`weighted-route-finder`](weighted-route-finder/README.md) | Find a minimum-time multi-edge route with Dijkstra's algorithm, stable tie-breaking, and predecessor-based path reconstruction. |
| Advanced | [`access-policy-review`](access-policy-review/README.md) | Evaluate role, action, and resource rules with deny-overrides, default-deny, and stable decision explanations. |
| Advanced | [`flood-evacuation-planner`](flood-evacuation-planner/README.md) | Assign whole households to safe shelters under deadlines, closed roads, hazard limits, accessible-space, and medical-support capacity constraints. |
| Advanced | [`insurance-claim-review`](insurance-claim-review/README.md) | Adjudicate fixed property claims through policy checks, duplicate-event prevention, per-claim deductibles, and policy-wide aggregate limits. |
| Advanced | [`package-lock-auditor`](package-lock-auditor/README.md) | Check a fixed dependency lock for missing packages, version-range conflicts, and cycles; produce a stable build order only when the graph is valid. |
| Advanced | [`supply-chain-recall-tracer`](supply-chain-recall-tracer/README.md) | Trace a contaminated component lot through manufacturing lineage, finished goods, shipped quantities, and retailer stock. |
| Advanced | [`ranked-choice-audit`](ranked-choice-audit/README.md) | Tally fixed ranked ballots by round, track exhausted preferences, and apply a documented tie rule. |
| Advanced | [`telemetry-frame-decoder`](telemetry-frame-decoder/README.md) | Validate and decode fixed sensor frames with byte masks, a checksum, and ordered structured errors. |
| Advanced | [`trial-cohort-auditor`](trial-cohort-auditor/README.md) | Audit a fictional study cohort for duplicate screening IDs, eligibility, visit windows, missing visits, and safety-report deadlines. |
| Advanced | [`webhook-delivery-audit`](webhook-delivery-audit/README.md) | Replay scripted delivery responses with capped backoff, terminal HTTP classification, attempt histories, and endpoint-scoped idempotency. |
| Advanced | [`air-quality-monitor`](air-quality-monitor/README.md) | Apply a PM2.5 alert state machine with consecutive-reading hysteresis, sensor-fault resets, and trigger/recovery thresholds. |
| Advanced | [`quota-ledger`](quota-ledger/README.md) | Model capacity-bounded reserve/release operations with contracts and property tests. |
| Advanced | [`nonogram-solver`](nonogram-solver/README.md) | Validate line clues, enumerate fixed-width candidates, prune partial grids, and backtrack to a deterministic solution. |
| Advanced | [`invoice-reconciliation`](invoice-reconciliation/README.md) | Match invoices against purchase orders and receipts; flag price and reference issues, partial billing, and cumulative overbilling. |
| Advanced | [`swiss-tournament-auditor`](swiss-tournament-auditor/README.md) | Validate a fixed Swiss-style tournament, calculate match points and Buchholz tie-breaks, then produce deterministic standings. |
| Advanced | [`municipal-microgrant-review`](municipal-microgrant-review/README.md) | Screen community project applications, rank a review rubric, and allocate whole grants under budget and district limits. |
| Advanced | [`wildlife-rescue-dispatch`](wildlife-rescue-dispatch/README.md) | Triage wildlife rescue calls and place animals at qualified clinics under permit, veterinarian, travel-radius, intake-slot, and treatment-hour constraints. |
| Advanced | [`museum-loan-readiness-review`](museum-loan-readiness-review/README.md) | Audit incoming museum loans for duplicate accessions, lender documents, condition, insurance, shipping crates, gallery humidity, and scarce display capacity. |
| Advanced | [`community-solar-credit-auditor`](community-solar-credit-auditor/README.md) | Reconcile shared solar generation against subscriber demand, contract caps, utility statements, and data-quality exceptions. |
| Advanced | [`festival-settlement-auditor`](festival-settlement-auditor/README.md) | Reconcile multi-ticket festival orders with processor captures, fees, refunds, and admission scans; release only records that pass checks. |
| Advanced | [`school-meal-service-planner`](school-meal-service-planner/README.md) | Choose an affordable school menu while checking allergy exclusions, nutrition bands, dated inventory, and kitchen capacity. |
| Advanced | [`digital-evidence-custody-auditor`](digital-evidence-custody-auditor/README.md) | Audit a custody event stream for actor permissions, seal and storage transitions, timestamp regressions, and final disposition. |
| Advanced | [`service-dependency-incident-correlator`](service-dependency-incident-correlator/README.md) | Rank service incident investigation leads by alert severity, transitive dependency impact, alert onset, and nearby deployments. |
| Advanced | [`transit-fare-cap-auditor`](transit-fare-cap-auditor/README.md) | Recalculate a bounded tap ledger with transfer windows and daily caps; hold malformed sequences and expose unmatched charges. |

Most of the more involved examples use fixed fixtures and in-memory
calculations to cover practical workflows without external services or
persistent state. `quota-ledger` is a pure library with generated property
cases instead of a command-line workflow. Each project README lists the
language features it illustrates and the compiler stages that are still
needed to build and run it. Fixture policy choices that merit domain review
are called out with their reasons in each sample's **Review required** section.
