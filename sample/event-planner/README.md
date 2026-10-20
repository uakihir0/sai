# Event Planner

Event Planner is a small in-memory planner for a one-day SAI community event.
It lists four sessions, tracks eight attendees, and reports first-come-first-
served confirmations, waitlists, open seats, and attendee schedule conflicts.

```text
event-planner sessions
Event sessions
09:00-09:45 Welcome to SAI (Main Hall)
  registered: 4, confirmed: 3, waitlisted: 1, open seats: 0
10:00-11:00 Parser lab (Lab A)
  registered: 3, confirmed: 2, waitlisted: 1, open seats: 0
10:30-11:15 Design clinic (Lab B)
  registered: 3, confirmed: 2, waitlisted: 1, open seats: 0
11:30-12:00 Career Q&A (Main Hall)
  registered: 3, confirmed: 3, waitlisted: 0, open seats: 1

event-planner summary
4 sessions and 8 attendees
confirmed registrations: 10, waitlisted registrations: 3, open seats: 1
confirmed schedule conflicts: 1

event-planner conflicts
Ren is confirmed for Parser lab (10:00-11:00) and Design clinic (10:30-11:15); the sessions overlap.
```

## Commands

- `sessions` lists session times, rooms, capacity use, waitlist size, and open seats.
- `registrations` shows each fixture registration and its current confirmation status.
- `summary` totals confirmed registrations, waitlisted registrations, open seats,
  and schedule conflicts between confirmed sessions.
- `conflicts` names confirmed registrations for the same attendee that overlap.

`data.sai` is the fixed event fixture. Registration `sequence` is the
first-come-first-served order; the first `capacity` registrations for each
session are confirmed, and later ones are waitlisted. A schedule conflict is
reported only when the attendee is confirmed in both overlapping sessions, so
waitlisted requests do not create conflicts. Sessions use start and end minutes
since midnight for overlap checks and display separate clock-time strings.
The sample is read-only and does not accept new registrations or save changes.

The package uses the `sai:process` command-line entry point and multiple
modules. It is illustrative: the current compiler does not yet support the
complete package, process, loop, formatting, and multi-module pipeline needed
to build and run it. The fixture and printed arithmetic are checked, but no
successful semantic or runtime check is claimed.
