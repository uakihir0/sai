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
| Intermediate | [`clockwork-escape-room`](clockwork-escape-room/README.md) | Replay a short, readable museum adventure with guarded state transitions, inventory, and a bounded story log. |
| Intermediate | [`greenhouse-freeze-alert`](greenhouse-freeze-alert/README.md) | Calibrate a signed tenths-of-a-degree sensor reading, then pass one direct helper call as an argument to another direct call. |
| Intermediate | [`open-source-license-review`](open-source-license-review/README.md) | Review a fixed component inventory for evidence gaps, conflicts, policy exceptions, and unresolved release findings. |
| Intermediate | [`reading-log`](reading-log/README.md) | Summarize a fixed book queue with status counts, page totals, and next-book selection. |
| Intermediate | [`flashcard-review-queue`](flashcard-review-queue/README.md) | Extend reading logs with due-card selection and recall-based intervals for an active review loop. |
| Intermediate | [`market-checkout`](market-checkout/README.md) | Review orders, aggregate repeated stock requests, and calculate discounts and tax. |
| Intermediate | [`home-energy-bill-auditor`](home-energy-bill-auditor/README.md) | Compare flat and time-of-use tariffs with solar credits, demand charges, bounded integer arithmetic, and tax. |
| Intermediate | [`library-loan-policy`](library-loan-policy/README.md) | Enforce checkout, hold, and renewal rules with records, enums, and `Result`. |
| Intermediate | [`marsh-survey-review`](marsh-survey-review/README.md) | Classify fixed field observations by correction, conservation review, confidence, and transect totals. |
| Intermediate | [`corpus-search-ranker`](corpus-search-ranker/README.md) | Rank a fixed text corpus with whitespace-token matching, weighted title scores, and stable tie-breaking. |
| Intermediate | [`life-grid-lab`](life-grid-lab/README.md) | Evolve a fixed 5×5 Conway's Game of Life board with bounded generations, finite dead boundaries, and neighbor-count rules. |
| Intermediate | [`facility-maintenance-queue`](facility-maintenance-queue/README.md) | Schedule a fixed facilities backlog across qualified technicians, shift windows, due times, and a shared parts cupboard. |
| Intermediate | [`neighborhood-bike-share-rebalancer`](neighborhood-bike-share-rebalancer/README.md) | Plan direct bike-share rebalancing trips by station need and donor inventory while preserving docks reserved for adaptive cycles. |
| Advanced | [`event-planner`](event-planner/README.md) | Apply session capacity and waitlists, then detect conflicts between confirmed sessions. |
| Advanced | [`community-radio-rundown`](community-radio-rundown/README.md) | Plan serial broadcast content around a protected break and hard sign-off with a minute-level content clock. |
| Advanced | [`university-enrollment-planner`](university-enrollment-planner/README.md) | Enforce year, program, prerequisite, credit, and timetable rules while ranking eligible section waitlists. |
| Advanced | [`turn-based-duel`](turn-based-duel/README.md) | Simulate stateful turns with combat rules, mutable state, and a deterministic opponent strategy. |
| Advanced | [`incident-response-console`](incident-response-console/README.md) | Score incidents, apply response deadlines, and identify SLA breaches and escalations. |
| Advanced | [`island-grid-dispatch`](island-grid-dispatch/README.md) | Allocate a constrained one-hour power budget by criticality while preserving a battery reserve. |
| Advanced | [`clinic-shift-roster`](clinic-shift-roster/README.md) | Validate clinician credentials, availability, shift caps, rest periods, and required coverage. |
| Advanced | [`care-transition-handoff-coordinator`](care-transition-handoff-coordinator/README.md) | Route fictional discharge cases through clinician-supplied clearance and ordered pharmacy, transport, home-support, and follow-up handoffs; assign each next action to one team. |
| Advanced | [`court-calendar-planner`](court-calendar-planner/README.md) | Schedule fixed hearings around deadlines, judges, courtrooms, parties, witnesses, and session capacity; report unscheduled cases. |
| Advanced | [`community-food-bank`](community-food-bank/README.md) | Allocate expiring pantry stock across household requests with dietary eligibility and priority rules. |
| Advanced | [`community-irrigation-rotation`](community-irrigation-rotation/README.md) | Plan drought-restricted garden watering across zone windows, field and canal losses, priority order, flow caps, and a weekly allotment. |
| Advanced | [`community-garden-harvest-planner`](community-garden-harvest-planner/README.md) | Match ripe garden beds to pantry pickup deadlines under priority order and daily harvest, wash-and-pack, and cold-chain throughput limits. |
| Advanced | [`community-repair-cafe-planner`](community-repair-cafe-planner/README.md) | Schedule community repair-cafe intake across safety holds, volunteer skills, appointment windows, workbench limits, and scarce shared parts. |
| Advanced | [`civic-service-request-triage`](civic-service-request-triage/README.md) | Route fictional resident service reports by category and urgency, link same-day duplicates, and calculate first-response targets while holding uncertain cases for review. |
| Advanced | [`construction-permit-review`](construction-permit-review/README.md) | Screen fictional building permits for evidence gaps and independent zoning, accessibility, and environmental review triggers. |
| Advanced | [`warehouse-fulfillment`](warehouse-fulfillment/README.md) | Reserve date-eligible inventory for prioritized orders, then choose a warehouse and route under cold-chain, delivery-deadline, and lane-capacity constraints. |
| Advanced | [`rail-shuttle-dispatch`](rail-shuttle-dispatch/README.md) | Assign a fixed replacement-bus fleet to disrupted transit services under corridor, passenger-capacity, and wheelchair-space constraints; report unresolved requests deterministically. |
| Advanced | [`railway-interlocking-auditor`](railway-interlocking-auditor/README.md) | Review fictional train-route reservations against protected blocks, turnout alignment, occupied tracks, overlap protection, and deterministic slot allocation. |
| Advanced | [`weighted-route-finder`](weighted-route-finder/README.md) | Find a minimum-time multi-edge route with Dijkstra's algorithm, stable tie-breaking, and predecessor-based path reconstruction. |
| Advanced | [`ipv4-subnet-planner`](ipv4-subnet-planner/README.md) | Allocate aligned IPv4 subnets from a fixed `/24` with batch validation, duplicate-ID rejection, deterministic size ordering, capacity reporting, and an independent overlap audit. |
| Advanced | [`virtual-memory-page-replacement-lab`](virtual-memory-page-replacement-lab/README.md) | Compare FIFO, LRU, and Clock page replacement with per-access frame states, dirty-page writebacks, and final flush accounting. |
| Advanced | [`bounded-static-linker`](bounded-static-linker/README.md) | Lay out bounded object sections, resolve local/weak/strong symbols, and validate absolute relocations before publishing an image manifest. |
| Advanced | [`bounded-expression-evaluator`](bounded-expression-evaluator/README.md) | Evaluate pre-tokenized integer expressions with precedence, parentheses, fixed-capacity stacks, source offsets, and checked arithmetic. |
| Advanced | [`bounded-protocol-model-checker`](bounded-protocol-model-checker/README.md) | Exhaustively explore a bounded two-client lease protocol with BFS; report shortest safety counterexamples, reachable deadlocks, and incomplete searches. |
| Advanced | [`bounded-gc-heap-lab`](bounded-gc-heap-lab/README.md) | Trace roots through cyclic object graphs, reclaim unreachable objects, and measure fragmentation before and after compaction in a fixed heap. |
| Advanced | [`bounded-wal-key-value-store`](bounded-wal-key-value-store/README.md) | Replay a fixed write-ahead journal with transaction commit markers, ignore interrupted writes, preserve tombstones, and compact to a canonical snapshot. |
| Advanced | [`two-phase-commit-lab`](two-phase-commit-lab/README.md) | Simulate coordinator commit and abort decisions, pre-log in-doubt participants, recorded decision replay, and partial delivery recovery. |
| Advanced | [`quorum-replication-lab`](quorum-replication-lab/README.md) | Explore three-replica read/write quorums, partially applied writes with lost acknowledgements, deterministic version selection, and bounded read repair. |
| Advanced | [`access-policy-review`](access-policy-review/README.md) | Evaluate role, action, and resource rules with deny-overrides, default-deny, and stable decision explanations. |
| Advanced | [`flood-evacuation-planner`](flood-evacuation-planner/README.md) | Assign whole households to safe shelters under deadlines, closed roads, hazard limits, accessible-space, and medical-support capacity constraints. |
| Advanced | [`insurance-claim-review`](insurance-claim-review/README.md) | Adjudicate fixed property claims through policy checks, duplicate-event prevention, per-claim deductibles, and policy-wide aggregate limits. |
| Advanced | [`package-lock-auditor`](package-lock-auditor/README.md) | Check a fixed dependency lock for missing packages, version-range conflicts, and cycles; produce a stable build order only when the graph is valid. |
| Advanced | [`supply-chain-recall-tracer`](supply-chain-recall-tracer/README.md) | Trace a contaminated component lot through manufacturing lineage, finished goods, shipped quantities, and retailer stock. |
| Advanced | [`ranked-choice-audit`](ranked-choice-audit/README.md) | Tally fixed ranked ballots by round, track exhausted preferences, and apply a documented tie rule. |
| Advanced | [`telemetry-frame-decoder`](telemetry-frame-decoder/README.md) | Validate and decode fixed sensor frames with byte masks, a checksum, and ordered structured errors. |
| Advanced | [`vaccine-cold-chain-release-auditor`](vaccine-cold-chain-release-auditor/README.md) | Audit fictional vaccine shipments against logger coverage, temperature excursions, calibration, custody evidence, lot expiry, and usable-stock limits. |
| Advanced | [`trial-cohort-auditor`](trial-cohort-auditor/README.md) | Audit a fictional study cohort for duplicate screening IDs, eligibility, visit windows, missing visits, and safety-report deadlines. |
| Advanced | [`webhook-delivery-audit`](webhook-delivery-audit/README.md) | Replay scripted delivery responses with capped backoff, terminal HTTP classification, attempt histories, and endpoint-scoped idempotency. |
| Advanced | [`air-quality-monitor`](air-quality-monitor/README.md) | Apply a PM2.5 alert state machine with consecutive-reading hysteresis, sensor-fault resets, and trigger/recovery thresholds. |
| Advanced | [`quota-ledger`](quota-ledger/README.md) | Model capacity-bounded reserve/release operations with contracts and property tests. |
| Advanced | [`nonogram-solver`](nonogram-solver/README.md) | Validate line clues, enumerate fixed-width candidates, prune partial grids, and backtrack to a deterministic solution. |
| Advanced | [`offline-field-sync-reconciler`](offline-field-sync-reconciler/README.md) | Three-way merge offline work-order edits by field; suppress exact retries and hold concurrent edits, stale bases, and event-ID collisions for review. |
| Advanced | [`invoice-reconciliation`](invoice-reconciliation/README.md) | Match invoices against purchase orders and receipts; flag price and reference issues, partial billing, and cumulative overbilling. |
| Advanced | [`swiss-tournament-auditor`](swiss-tournament-auditor/README.md) | Validate a fixed Swiss-style tournament, calculate match points and Buchholz tie-breaks, then produce deterministic standings. |
| Advanced | [`municipal-microgrant-review`](municipal-microgrant-review/README.md) | Screen community project applications, rank a review rubric, and allocate whole grants under budget and district limits. |
| Advanced | [`affordable-housing-allocation`](affordable-housing-allocation/README.md) | Allocate a fixed rental-home inventory to eligible households under income, rent, occupancy, accessibility, and preference rules. |
| Advanced | [`wildlife-rescue-dispatch`](wildlife-rescue-dispatch/README.md) | Triage wildlife rescue calls and place animals at qualified clinics under permit, veterinarian, travel-radius, intake-slot, and treatment-hour constraints. |
| Advanced | [`museum-loan-readiness-review`](museum-loan-readiness-review/README.md) | Audit incoming museum loans for duplicate accessions, lender documents, condition, insurance, shipping crates, gallery humidity, and scarce display capacity. |
| Advanced | [`community-solar-credit-auditor`](community-solar-credit-auditor/README.md) | Reconcile shared solar generation against subscriber demand, contract caps, utility statements, and data-quality exceptions. |
| Advanced | [`facility-ghg-inventory-auditor`](facility-ghg-inventory-auditor/README.md) | Audit a multi-site annual greenhouse-gas inventory across organizational boundaries, evidence IDs, factor validity, checked arithmetic, and separate Scope 2 location- and market-based totals. |
| Advanced | [`coffee-roastery-production-auditor`](coffee-roastery-production-auditor/README.md) | Audit fixed roast batches against order demand, green-bean lot balances, organic certification, roast profiles, and roaster charge limits. |
| Advanced | [`festival-settlement-auditor`](festival-settlement-auditor/README.md) | Reconcile multi-ticket festival orders with processor captures, fees, refunds, and admission scans; release only records that pass checks. |
| Advanced | [`school-meal-service-planner`](school-meal-service-planner/README.md) | Choose an affordable school menu while checking allergy exclusions, nutrition bands, dated inventory, and kitchen capacity. |
| Advanced | [`digital-evidence-custody-auditor`](digital-evidence-custody-auditor/README.md) | Audit a custody event stream for actor permissions, seal and storage transitions, timestamp regressions, and final disposition. |
| Advanced | [`event-time-stream-aggregator`](event-time-stream-aggregator/README.md) | Aggregate out-of-order sensor readings into tumbling windows with a shared watermark, late-event findings, and deterministic closure. |
| Advanced | [`expense-reimbursement-auditor`](expense-reimbursement-auditor/README.md) | Audit travel claims against employee trips, receipt evidence, duplicate receipt references, per-diem budgets, and category reimbursement limits. |
| Advanced | [`service-dependency-incident-correlator`](service-dependency-incident-correlator/README.md) | Rank service incident investigation leads by alert severity, transitive dependency impact, alert onset, and nearby deployments. |
| Advanced | [`transit-fare-cap-auditor`](transit-fare-cap-auditor/README.md) | Recalculate a bounded tap ledger with transfer windows and daily caps; hold malformed sequences and expose unmatched charges. |
| Advanced | [`regional-storm-coordination`](regional-storm-coordination/README.md) | Coordinate road clearance, shelter power, accessible evacuation, medical transport, water delivery, and radio relay work across prerequisite and unit-capacity constraints. |
| Advanced | [`backup-restore-readiness-auditor`](backup-restore-readiness-auditor/README.md) | Validate full and incremental backup chains against RPO/RTO, retention, checksum, and offsite requirements; select the newest compliant restore point and report fallbacks. |
| Advanced | [`canary-release-safety-planner`](canary-release-safety-planner/README.md) | Order dependent service releases across regional canary and general cohorts; apply health gates, halt on failed evidence, and record rollback decisions deterministically. |
| Advanced | [`port-call-coordinator`](port-call-coordinator/README.md) | Greedily schedule fictional vessel calls against berths, tide windows, cranes, reefer plugs, hazmat separation, and a shared one-way channel; reject malformed input and explain unplaced calls. |
| Advanced | [`observatory-night-planner`](observatory-night-planner/README.md) | Schedule fictional telescope requests under target visibility, dark-time, filter, guiding, cloud, and setup-time constraints with a deterministic priority policy. |
| Advanced | [`orbital-conjunction-review`](orbital-conjunction-review/README.md) | Rank fictional satellite conjunction alerts by uncertainty-adjusted miss distance and review maneuver timing, spacecraft limits, and protected propellant reserves. |
| Advanced | [`order-fulfillment-saga`](order-fulfillment-saga/README.md) | Replay multi-step order events with operation-key idempotency, bounded retries, reverse-order compensation, and partial-failure audit states. |
| Advanced | [`package-version-resolver`](package-version-resolver/README.md) | Resolve bounded version domains by constraint propagation and deterministic backtracking, reporting search counts and unsatisfiable outcomes. |
| Advanced | [`accessible-audio-release-auditor`](accessible-audio-release-auditor/README.md) | Audit a fictional audiobook release for required audio parts, chapter synchronization, timed transcript and description coverage, version consistency, and accessibility approvals. |
| Advanced | [`privacy-request-coordinator`](privacy-request-coordinator/README.md) | Coordinate a fictional access and erasure request across stores with identity and scope gates, legal holds, retention, dependency-ordered deletions, retries, and auditable partial results. |
| Advanced | [`smart-building-comfort-controller`](smart-building-comfort-controller/README.md) | Plan requested setpoints and ventilation under comfort bands, target hysteresis, price and carbon budgets, equipment limits, and visible unmet requests. |
| Advanced | [`municipal-procurement-bid-review`](municipal-procurement-bid-review/README.md) | Review fictional public-works bids through submission and supplier gates, unit-price arithmetic correction, a technical floor, normalized best-value scoring, and a deterministic award recommendation. |
| Advanced | [`reproducible-pipeline-planner`](reproducible-pipeline-planner/README.md) | Validate workflow dependencies and schedule ready tasks in deterministic batches under worker and shared-resource limits, propagating invalid or failed prerequisites. |
| Advanced | [`mentorship-matching`](mentorship-matching/README.md) | Match fictional mentees to mentors with bounded deferred acceptance, topic eligibility, capacities, strict preferences, and an auditable proposal trail. |
| Advanced | [`multi-region-failover-planner`](multi-region-failover-planner/README.md) | Plan dependency-ordered service recovery waves under replica RPO freshness, dependency-aware RTO, ranked regional targets, capacity reservations, and explicit holds. |
| Advanced | [`tool-library-loan-scheduler`](tool-library-loan-scheduler/README.md) | Schedule multi-day community tool loans against returning stock, repair capacity, safety badges, member loan limits, duplicate holds, and ordered rejection reasons. |
| Advanced | [`urban-tree-planting-planner`](urban-tree-planting-planner/README.md) | Match urban planting sites to tree species under soil and clearance limits, district diversity, nursery stock, and a shared establishment-watering budget. |
| Advanced | [`spaceflight-mission-operations`](spaceflight-mission-operations/README.md) | Schedule a fictional spacecraft timeline across prerequisite graphs, crew windows, and overlapping power and thermal budgets, with explicit blocked and unplaced outcomes. |
| Advanced | [`key-rotation-readiness-auditor`](key-rotation-readiness-auditor/README.md) | Audit fictional service-key rotations for lifetime coverage, migration evidence freshness, escrow separation, overlap windows, and in-plan dependency order. |
| Advanced | [`battery-recycling-chain-auditor`](battery-recycling-chain-auditor/README.md) | Audit fictional battery returns through custody, transport permits and quotas, assay quality, available material, destination licenses, and mass balance. |
| Advanced | [`instrument-calibration-traceability-auditor`](instrument-calibration-traceability-auditor/README.md) | Check calibration windows and instrument scope while tracing reference-standard chains for accreditation, recall, range, and conservative uncertainty. |
| Advanced | [`drinking-water-incident-coordinator`](drinking-water-incident-coordinator/README.md) | Coordinate fictional drinking-water incidents from qualified sensor and lab evidence through affected-zone advisories, alternate supply, and recovery checks. |
| Advanced | [`research-data-release-auditor`](research-data-release-auditor/README.md) | Gate research dataset releases on source lineage, contributor consent, disclosure risk, integrity, retention, embargoes, legal holds, and parent decisions. |
| Advanced | [`digital-archive-preservation-planner`](digital-archive-preservation-planner/README.md) | Prioritize fixed-fixture digital archive migrations and replicas under fixity, permission, independent-copy, byte-budget, and task-limit rules. |
| Advanced | [`api-rate-limit-simulator`](api-rate-limit-simulator/README.md) | Replay per-client API requests against independent fixed-window limits, including exact-boundary resets, retry times, and unknown clients. |
| Advanced | [`resilience-drill-planner`](resilience-drill-planner/README.md) | Plan fictional tabletop drills by recalculated marginal failure-scenario coverage under readiness, team-cap, time-budget, and drill-count limits. |
| Advanced | [`transaction-serializability-auditor`](transaction-serializability-auditor/README.md) | Analyze bounded read/write histories, build a precedence graph, and report a deterministic serial order or cycle witness. |
| Advanced | [`carbon-aware-compute-window-planner`](carbon-aware-compute-window-planner/README.md) | Schedule bounded compute jobs across regional windows with deadline, locality, capacity, and carbon-budget constraints; hold uncertain inputs for review. |
| Advanced | [`bounded-dns-cache-resolver`](bounded-dns-cache-resolver/README.md) | Resolve fixed DNS records through bounded CNAME chains and a positive/negative TTL cache with deterministic LRU eviction. |
| Advanced | [`robust-statistics-lab`](robust-statistics-lab/README.md) | Summarize fixed sensor batches with median and median absolute deviation, bounded arithmetic, and an explicit zero-MAD outlier policy. |
| Advanced | [`bounded-bytecode-verifier`](bounded-bytecode-verifier/README.md) | Verify typed operand stacks across bounded control flow, including branch targets, stack-shape and value-kind joins, and return paths. |
| Advanced | [`cluster-capacity-planner`](cluster-capacity-planner/README.md) | Place fixed service replicas across node pools under CPU and memory headroom, safety reserves, deterministic priority, best fit, and failure-zone spread rules. |
| Advanced | [`workbook-recalculator`](workbook-recalculator/README.md) | Recalculate a fixed workbook with dependency passes, cycle detection, stable formula order, and local error propagation. |

## Longer walkthroughs

- [`railway-interlocking-auditor`](railway-interlocking-auditor/README.md)
  combines deterministic request ordering with protected block reservations
  and turnout conflicts.
- [`vaccine-cold-chain-release-auditor`](vaccine-cold-chain-release-auditor/README.md)
  carries sensor coverage and excursion findings through evidence review,
  shipment decisions, and inventory accounting.
- [`order-fulfillment-saga`](order-fulfillment-saga/README.md) replays
  idempotent operations and reverse-order compensation after partial failure.
- [`reproducible-pipeline-planner`](reproducible-pipeline-planner/README.md)
  validates task dependencies and schedules bounded deterministic batches.
- [`care-transition-handoff-coordinator`](care-transition-handoff-coordinator/README.md)
  routes each fictional case's first unresolved required handoff to its owning
  team while keeping clinical clearance outside the coordinator's authority.
- [`digital-archive-preservation-planner`](digital-archive-preservation-planner/README.md)
  validates preservation evidence, ranks migrations and replicas, and fits
  work to separate byte and task budgets while retaining catalog order.
- [`transaction-serializability-auditor`](transaction-serializability-auditor/README.md)
  builds a precedence graph from a bounded history and returns a stable cycle
  witness when no serial order exists.
- [`bounded-bytecode-verifier`](bounded-bytecode-verifier/README.md)
  propagates typed abstract stacks through a FIFO control-flow worklist and
  checks branch joins and return shapes.
- [`cluster-capacity-planner`](cluster-capacity-planner/README.md)
  places priority-ordered service replicas under two-dimensional capacity,
  reserved headroom, pool eligibility, and zone-spread constraints.
- [`robust-statistics-lab`](robust-statistics-lab/README.md)
  compares median/MAD summaries with an explicit threshold and zero-MAD policy
  for a small fixed sensor dataset.
- [`workbook-recalculator`](workbook-recalculator/README.md)
  evaluates bounded formula dependencies, detects cycles, and reports errors
  without blocking unrelated cells.

Most of the more involved examples use fixed fixtures and in-memory
calculations to cover practical workflows without external services or
persistent state. `quota-ledger` is a pure library with generated property
cases instead of a command-line workflow. Each project README lists the
language features it illustrates and the compiler stages that are still
needed to build and run it. Fixture policy choices that merit domain review
are called out with their reasons in each sample's **Review required** section.
