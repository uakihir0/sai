# Municipal Procurement Bid Review

This fixed-fixture library evaluates bids for one fictional city street-safety
contract. It records intake and supplier gates, corrects quoted line extensions
from the published unit prices, applies a minimum technical score and a total
contract ceiling, then ranks only bids that pass every gate under a documented
best-value formula. Its result is a review and award recommendation, not an
award or contract.

## Hand-checked fixture result

The intended result of `procurement_report()` is:

| Bid ID | Supplier | Evaluated cost ($000) | Technical | Result |
|---:|---|---:|---:|---|
| 7307 | North Beacon Utilities | 840 | 52 | Rank 3; 92.00 points |
| 7304 | Greenway Streets | 700 | 41 | Excluded: below 42-point technical floor |
| 7302 | Harborline Civil | 840 | 52 | Corrected from $830,000; rank 2 at 92.00 |
| 7309 | Civic Works Cooperative | 920 | 56 | Recommended; rank 1 at 92.52 |
| 7306 | Highline Paving | 1,600 | 60 | Excluded: above $1,500,000 ceiling |
| 7303 | East Ward Contracting | 780 | 59 | Excluded: received after the deadline |
| 7305 | MetroGrid Services | 820 | 58 | Excluded: addendum not acknowledged |
| 7311 | Riverfront Infrastructure | 800 | 57 | Held: unresolved conflict review |
| 7310 | Portage Contractors | 960 | 54 | Excluded: supplier suspended |
| 7312 | Parkway Maintenance | 900 | 61* | Excluded: delivery score exceeds rubric maximum |
| 7313 | Cedar City Builders | 860 | 58 | Excluded: bid security missing |
| 7314 | Market Renewal LLC | 880 | 59 | Excluded: vendor registration is not current |
| 7315 | West City Engineering | 875 | 57 | Excluded: signed form missing |
| 7316 | Zero Bid Demonstrator | 0 | 60 | Excluded: evaluated price must be positive |

The price baseline is $840,000, the lowest corrected price among the three
qualified bids. Greenway Streets' lower $700,000 offer does not set that
baseline because its technical score is below the published floor. Civic
Works Cooperative is recommended at a corrected evaluated amount of $920,000:
its 56 technical points plus 36.52 price points yield 92.52 overall. This
example uses a best-value policy, so the recommendation is not simply the
lowest-priced bid.

Harborline's lighting unit price is $80,000 for two units, while its submitted
extension is $150,000. The published unit-price-controls rule corrects that
extension to $160,000, making the evaluated total $840,000 rather than the
submitted $830,000. Harborline and North Beacon tie on overall score, technical
score, and corrected price. Harborline ranks first between them because its
receipt time is earlier.

## Review flow and scoring

1. `fixtures.sai` supplies 14 deliberately unsorted bids, the fixed contract
   quantities, submission deadline, ceiling, and technical score limits.
2. `intake.sai` calculates corrected prices from unit prices and applies gates
   in order: deadline, signed form, addendum acknowledgment, bid security,
   current vendor registration, suspension, unresolved conflict, positive
   evaluated price, valid rubric inputs, technical floor, and cost ceiling.
   The first failed intake gate is the displayed stage; the original bid
   record remains attached to the result.
3. `evaluation.sai` marks any later duplicate bid ID as excluded, finds the
   lowest qualified corrected price, calculates price and combined scores,
   ranks qualified bids, and marks one recommendation. It preserves input
   order in the final report.
4. `lib.sai` exposes `procurement_report()` and assertions for price-baseline
   eligibility, arithmetic correction, tied bids, recommendation, duplicate
   identifiers, overflow-range scores, and failed gates.

Technical review totals up to 60 points: delivery plan (25), work quality (20),
and accessibility (15). The components are widened before summation so
out-of-range values can be rejected without overflowing. A bid needs at least
42 points. Price contributes up to 40 points, represented in hundredths of a
point:

```text
price score = floor(4,000 × lowest qualified evaluated price / bid evaluated price)
combined score = 100 × technical points + price score
```

For exact ties, the published order is higher technical score, lower corrected
evaluated price, earlier receipt time, then lower bid ID. Prices are stored in
whole thousands of dollars. The fixed scope requires four paving units, two
lighting units, and one crossing treatment.

## Project structure and concepts

- `src/model.sai` defines bids, document and supplier records, staged outcomes,
  evaluations, and the final report.
- `src/fixtures.sai` contains 14 fictional bids, including a corrected
  extension, a zero-price schedule, a low technical score, a cap breach,
  intake defects, and responsibility holds.
- `src/intake.sai` performs ordered pass/fail review and unit-price arithmetic
  correction using bounded integer calculations.
- `src/evaluation.sai` applies normalized price scoring, deterministic
  tie-breaking, ranking, and award recommendation.
- `src/lib.sai` exposes the report and source-level regression assertions.

The example illustrates records, enums, fixed arrays, `Option`, module
boundaries, loops, bounded integer arithmetic, multi-stage review, score
normalization, explainable outcomes, and stable ranking. It is a pure
in-memory library with no external dependencies, file access, or payment
integration.

## Assumptions and limits

All bidders, records, scores, prices, and times are fictional. Among intake
gates, the first failure is the primary displayed outcome. A duplicate-ID
finding supersedes that outcome for each later occurrence; the original input
is retained so reviewers can inspect its other submitted fields. An unresolved
conflict is held from ranking until human review clears it. The unit price
controls a line-extension mismatch, and all three published quantities are
mandatory in the evaluated total. The price score uses integer floor rounding.
The evaluator retains source fixture order instead of presenting bids in rank
order; each qualified row carries its computed rank.

The project does not inspect source documents, verify a supplier database,
adjudicate an actual conflict, detect collusion, validate real procurement law,
or authorize an award. The scores are fictional reviewer inputs, not an
objective measurement of public value. Currency, quantities, deadline, ceiling,
gates, technical floor, weights, arithmetic rule, and tie-break order must be
replaced with the governing solicitation and applicable law before any
real-world use. The fixture assumes receipt times are comparable minutes on
one date. A repeated bid ID is rejected on its later occurrence so it cannot
create multiple first-place recommendations; production intake must still
validate identifiers at the source. All fixture IDs are unique. This is not
legal, procurement, or engineering advice.

## Review required

- The one-time conflict hold blocks ranking until a reviewer resolves it. This
  fail-closed treatment reduces the risk of a conflict escaping notice but can
  delay an otherwise responsive bid; a real process must define recusal,
  disclosure, investigation, and clearance authority.
- Price contributes 40% and technical quality contributes 60%, with a
  42-point technical floor. These weights and the three technical categories
  are invented teaching policy. The scoring method must be published in the
  solicitation and approved for its procurement method.
- The unit-price-controls rule recomputes every extension from fixed
  solicitation quantities. Some jurisdictions or solicitations prescribe
  different correction rules, notification duties, or bidder acceptance rights.
- The sample rejects a bid whose corrected total is zero so that the
  price-scoring denominator is always positive. A zero-priced line can still
  be part of a positive total. Whether either treatment is permissible depends
  on the actual solicitation.
- A registration record and suspension flag are trusted fixture inputs. The
  sample does not verify their source or address appeals, waivers, or
  responsibility determinations.
- The $1,500,000 cost ceiling is a hard eligibility gate. A real procurement
  must specify whether a ceiling is public, how options and lifecycle costs are
  included, and whether negotiation or best-and-final offers are permitted.
- The report makes a single recommendation only after deterministic review.
  It does not substitute for a procurement officer's independent review,
  approval, notice, protest window, or executed contract.

## Package and compiler status

The manifest targets `wasm32-core`, declares no external package, and requests
no runtime capabilities. The expected table and regression assertions are
hand-checked against the fixture; they are not compiler-generated results. The
current compiler does not yet support all Stage 0 `sai check`, `@derive`, and
Stage 3 `sai test` requirements, so type correctness and executable behavior
remain unverified.
