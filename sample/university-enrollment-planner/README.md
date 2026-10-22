# University Enrollment Planner

This fixed Spring term fixture shows how student standing, course rules, credits,
meeting times, section capacity, and waitlist priority can affect one another.
It is a pure multi-module SAI library: all records are declared in source, and
planning performs no file, network, database, or other external I/O.

The decisions below are the **hand-calculated expected fixture result**. They
are not program output and do not claim that the sample has been built or run.

```text
University term plan — Spring (expected fixture result)

Aki Tanaka (year 4, 7-credit cap)
  CS-401 Capstone Studio: enrolled (4 credits)
  CS-301 AI Lab: enrolled (3 credits)
  CS-220 Databases: rejected — term credit limit (10 would exceed 7)

Noa Sato (year 4, Arts)
  CS-301 AI Lab: rejected — Computer Science program required

Mio Kato (year 3, 5-credit cap)
  CS-301 AI Lab: waitlist #1
  CS-220 Databases: enrolled (3 credits)
  CS-201 Data Structures: rejected — timetable conflict with CS-220

Aya Mori (year 3)
  CS-301 AI Lab: waitlist #2
  CS-201 Data Structures: enrolled (2 credits)
  CS-220 Databases: rejected — timetable conflict with CS-201

Sam Ito (year 2)
  CS-201 Data Structures: rejected — prior completion of CS-101 required

Ren Watanabe (year 2)
  CS-201 Data Structures: waitlist #1
  CS-220 Databases: waitlist #1

Yui Nakamura (year 1)
  CS-201 Data Structures: rejected — minimum year 2 required

Summary: 4 enrolled, 4 waitlisted, 6 rejected
Section use: CS-101 0/2, CS-201 1/1, CS-220 1/1,
             CS-301 1/1, CS-401 1/1
Accepted term credits: Aki 7/7, Mio 3/5, Aya 2/7
```

## Rules and fixed assumptions

- Course year is the priority key: seniors are processed before juniors,
  sophomores, then first-year students. The planner sorts requests by descending
  student year, then ascending request sequence. The fixture is intentionally
  stored out of order so the priority pass is visible in the source.
- A student's request sequence also gives their fixed preference order. For
  students at the same year, earlier sequence breaks the tie. This is a small
  deterministic policy, not a claim about any university's registration rules.
- Program eligibility, minimum year, and prerequisites are checked before
  credits, timetable conflicts, and seats. A prerequisite must appear in that
  student's prior-term completion list; a course in the current plan cannot
  satisfy another course's prerequisite.
- Only earlier enrolled courses consume a student's credit limit or timetable.
  A waitlisted request is passive: it does not reserve credits or create a time
  conflict, and this example does not automatically promote waitlisted students.
- Eligible requests enter a section waitlist only when its single fixed section
  is full. Waitlist positions follow the same year-and-sequence priority order.
  Rejected requests do not consume seats.
- Each course has one section in this fixture. Days use Monday = 1 through
  Sunday = 7; times are minutes after midnight. Meeting intervals are
  half-open, so a class ending at 10:00 does not conflict with one beginning
  then.

## Project structure and concepts

- `model.sai` defines students, course requirements, sections, requests, and
  decisions.
- `data.sai` contains seven students, five courses and sections, and fourteen
  deliberately unsorted requests.
- `policy.sai` provides fixed-data lookups and half-open interval overlap.
- `planner.sai` orders requests, applies academic eligibility, credit, and
  timetable checks, reserves seats, and assigns stable waitlist positions.
- `lib.sai` exposes `spring_term_plan()` as the pure library entry point.

## Package and compiler status

The package targets `wasm32-core`, has no external dependencies, and declares no
runtime capabilities. The sample includes focused Stage 3 `test` declarations
for request ordering, counts, waitlist positions, academic rejection reasons,
waitlisted-credit handling, timetable conflicts, interval boundaries, and
section capacity. The current
compiler's Stage 0 `sai check` and `@derive` feature gates prevent semantic
compilation, and Stage 3 `sai test` is not implemented. `sai fmt --check sample`
parses and formats the declarations, but the source tests have not been
executed. Type correctness and runtime behavior are not verified; the table
above remains a manually checked expected result only.

The fixture is fictional and intentionally bounded. It assumes unique request
sequences, known IDs for its fixed records, and no duplicate request for one
student/course pair. It does not model add/drop windows, cross-listed or
multiple sections, course repeats, term-to-term prerequisite chains, concurrent
seat claims, persistence, tuition, or real institutional registration policy.
