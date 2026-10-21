# Clinic Shift Roster

Clinic Shift Roster reviews a fixed proposal for three clinic shifts across
two days. Each shift needs one charge nurse and one physician with the required
credential. The report checks staff qualifications, availability, maximum
shift counts, and a minimum rest period between assignments.

```text
clinic-shift-roster roster
Clinic roster proposal — Northside

Day 1 early (06:00-14:00)
  charge nurse: Mina
  physician (trauma): Dr. Hara
Day 1 late (14:00-22:00)
  charge nurse: Ren
  physician (emergency): Dr. Mori
Day 2 early (06:00-14:00)
  charge nurse: Mina
  physician (emergency): Dr. Sato

Standby review
  Dr. Mori replacing Dr. Sato for Day 2 early physician: rejected (rest below minimum; 480 min rest; minimum 600)
```

```text
clinic-shift-roster summary
Roster summary
3 shifts; 6 proposed assignments; 0 invalid assignments
6 of 6 required positions covered
minimum rest: 600 min; standby Dr. Mori has 480 min before Day 2 early
```

## Commands

- `roster` prints each proposed assignment and the standby eligibility review.
- `coverage` shows whether each required position has exactly one valid
  assignment.
- `summary` totals shifts, proposed and invalid assignments, and covered
  positions.

## Roster rules

The fixture lists one proposed clinician for each position. A proposal is valid
only when the clinician is qualified for the position, is available for the
shift, is assigned to no more than their shift cap, has at least 600 minutes
between this and every other proposed assignment, and is the only proposal for
that shift and position. A position counts as covered only when it has exactly
one valid proposal. Rest is measured on a linear minute timeline: Day 2 starts
at minute 1,440, so the gap from Day 1 late's 22:00 end to Day 2 early's 06:00
start is 480 minutes.

The standby review checks Dr. Mori as a replacement for Dr. Sato in the Day 2
early physician position. It checks Mori's credential, availability, shift
cap, and rest against the fixed rota; the first failed constraint is reported.
Mori has the emergency credential, is available, and is below the two-shift
cap, but has only 480 minutes after the Day 1 late shift. The 600-minute rest
rule rejects the replacement trial. This is a candidate check, not a search
or recalculation of alternative rosters.

The schedule and the single standby trial are fixed fixtures. The program
validates them; it does not search for replacements, optimize preferences,
resolve labor agreements, or publish assignments. Credential and availability
rules are teaching choices, not clinical staffing guidance.

## Package and compiler status

The package targets `wasm32-wasi` and uses the `sai:process` command-line entry
point shape. The `sai:process` Git URL is currently unavailable, so resolution
needs a matching toolchain bundle index or a published source repository. This
example is illustrative: the current compiler does not yet support the full
package, `sai:process`/WASI entry point, command-line, loop, formatting,
`@derive`, and multi-module pipeline used here. The fixture arithmetic and
reported rest interval are internally consistent, but no successful check,
build, or run is claimed. The command dispatcher assumes the host includes
the executable name in `argv[0]`.
