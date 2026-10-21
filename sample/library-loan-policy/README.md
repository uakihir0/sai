# Library Loan Policy

Library Loan Policy models the rules around borrowing and renewing one library
copy. It accepts a member and copy snapshot and returns either a loan or the
first policy reason that rejects the request.

```text
checkout(active member, available copy, day 20)
  -> loan due on day 41

checkout(suspended member, available copy, day 20)
  -> rejected: MemberSuspended

checkout(active member at loan limit, available copy, day 20)
  -> rejected: LoanLimitReached

checkout(other active member, copy held for member 1, day 20)
  -> rejected: ReservedForAnotherMember(1)

renew(loan due on day 31, day 20, no waiting holds)
  -> loan due on day 52; one renewal used
```

`src/model.sai` defines the member, copy, loan, and error records and enums.
`src/policy.sai` applies checkout and renewal rules, `src/fixtures.sai` supplies
the scenarios, and `src/lib.sai` contains boundary tests.

Checkout checks account suspension, overdue loans, the member's loan limit,
copy availability, reservation ownership, and date overflow in that order.
A copy with a waiting reservation can only be checked out by the reserved
member. A loan lasts 21 integer days and permits one renewal. Renewal rejects
returned or overdue loans, an already renewed loan, and a loan with a waiting
reservation. An integer day index is used instead of calendar dates; no
calendar, branch, fine, or multi-copy queue is modeled.

## Package and compiler status

This pure `wasm32-core` library avoids process I/O and external packages. It
illustrates Stage 1 record and enum values, `Option`, `Result`, and `match`;
Stage 2 derived traits via `@derive`; and Stage 3 `test` declarations. The
current reference CLI still gates `sai check` at Stage 0 and `sai test` at
Stage 3. The sample is illustrative: no semantic check, build, or test
execution is claimed.
