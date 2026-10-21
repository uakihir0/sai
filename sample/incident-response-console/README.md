# Incident Response Console

Incident Response Console is a read-only triage report over five fixed
operational incidents. It combines severity and customer impact into a
priority score, calculates first-response targets, reports pending or late
responses, and lists active escalations.

```text
incident-response-console incidents
Incident triage
INC-2401 [P1 score 70] critical / widespread — Payments API: checkout requests time out
  first response: pending; target 15 min; overdue by 7 min
  escalation: incident commander (P1, immediate)
INC-2402 [P2 score 50] high / many customers — Identity: login retries are rising
  first response: met in 12 min (target 30 min)
INC-2403 [P3 score 30] medium / single customer — Search: one tenant sees stale results
  first response: pending; target 60 min; due in 5 min
INC-2404 [P3 score 40] high / single customer — Notifications: delivery is delayed
  first response: late at 75 min (target 60 min; exceeded by 15 min)
INC-2405 [P4 score 10] low / internal only — Build pipeline: nightly jobs are queued
  first response: pending; target 240 min; overdue by 20 min
  escalation: duty manager (P4, 20 min overdue)

incident-response-console summary
Triage summary
5 incidents: 1 P1, 1 P2, 2 P3, 1 P4
3 incidents awaiting a first response; 3 response SLA breaches
2 active escalations: 1 incident commander, 1 duty manager

incident-response-console escalations
Active escalations
INC-2401 -> incident commander (P1, immediate)
INC-2405 -> duty manager (P4, 20 min overdue)
```

## Commands

- `incidents` prints each incident's inputs, calculated score and priority,
  first-response status, and any active escalation.
- `escalations` lists incidents that need immediate or overdue escalation.
- `summary` counts incidents by priority, pending responses, SLA breaches, and
  active escalation destinations.

## Triage rules

The score adds a severity weight (critical 40, high 30, medium 20, low 10) to
an impact weight (widespread 30, many customers 20, single customer 10,
internal only 0). Scores of 60 or more are P1, 45–59 are P2, 25–44 are P3,
and lower scores are P4. First-response targets are 15, 30, 60, and 240
minutes for P1 through P4.

An incident without a first response is overdue only after its target has
elapsed. An unacknowledged P1 is escalated immediately to the incident
commander; another unacknowledged incident is escalated to the duty manager
after its target. A response received after its target counts as an SLA breach
but is not listed as an active escalation. The fixture therefore has three
SLA breaches: INC-2401 is seven minutes overdue, INC-2404 was answered 15
minutes late, and INC-2405 is 20 minutes overdue. Only INC-2401 and INC-2405
still need escalation.

`data.sai` is a static snapshot. The console does not connect to monitoring,
page responders, change incident state, or persist data. Its scores and
thresholds are teaching choices rather than a production incident policy.

## Package and compiler status

The package targets `wasm32-wasi` and uses the `sai:process` command-line
entry-point shape. The `sai:process` Git URL is currently unavailable, so
package resolution needs a matching toolchain bundle index or a published
source repository. This sample is illustrative: the current compiler does not
yet support the complete package, `sai:process`/WASI entry point, command-line,
loop, `@derive`, f-string formatting, and multi-module pipeline used here. The
arithmetic in the output above is internally consistent, but no successful
check, build, or run is claimed. The command dispatcher assumes the host
includes the executable name in `argv[0]`.
