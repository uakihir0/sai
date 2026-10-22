# Access Policy Review

Access Policy Review evaluates a fixed set of role-based rules for a principal
and one requested action on one resource. The policy uses **deny overrides**:
any matching deny wins over every matching allow. If no rule matches, access is
denied by default. The result identifies the rule responsible for an explicit
decision, or reports that no rule matched. Each decision carries the principal
ID and request that were evaluated.

```text
Support + Supervisor / Delete Incident
DENY — rule 103 explicitly denies this action

Support / Update Incident
ALLOW — rule 102 explicitly allows this action

Auditor / Update Incident
DENY — no rule matches; default deny
```

When several matching rules have the same effect, the decision reports the
lowest rule ID. This keeps the explanation stable if fixture order changes.
The fixtures include a principal with both Support and Supervisor roles so an
explicit Support denial overrides a Supervisor allowance.

`model.sai` defines principals, roles, requests, rules, and decisions.
`fixtures.sai` contains the fixed principals and policy. `policy.sai` matches
roles, actions, and resources and combines the results. `lib.sai` exposes
read-only evaluation functions and tests for allow, deny override, default
deny, and stable rule selection.

## Package and compiler status

This is a pure `wasm32-core` library. It does not authenticate a user, fetch
group membership, read policy files, or enforce permissions in an application.
It allows at most two fixed role slots per principal and eight fixed policy
rules; unused slots are `None`. Oversized counts return `PolicyInputError`
before array access, and the fixtures assume unique rule IDs.

This project illustrates role-based authorization, matching across principal,
action, and resource, deny-overrides composition, default-deny behavior, and
explainable decisions. It is illustrative rather than a build guarantee: the
current compiler has not completed the semantic and package pipeline needed
to build this source.
