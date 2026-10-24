# Accessible Audio Release Auditor

Accessible Audio Release Auditor checks a fictional audiobook excerpt before
distribution. It reviews the catalog fields and rights flag, requires a
narration and audio-description part for every chapter, checks each part
against the chapter time map, validates transcript and caption timing, and
requires current editorial, accessibility, and audio-post approvals.

The fixture is a 90-second, three-chapter preview of *The Last Ferry at
Moonrise*. The report returned by `sample_report()` is hand-checked from the
fixed inputs:

```text
Accessible audio release audit
Release: The Last Ferry at Moonrise (AUD-204) — en-GB — version 2.4.1
Decision: HOLD — resolve the findings before distribution
Metadata: PASS — required catalog, title, narrator, locale, package version, advisory, and rights fields
Required audio parts: 6/6 present
Present-part boundary sync: PASS — 250 ms tolerance
Narration gaps: REVIEW — 1 gap(s) exceed 100 ms
Transcript coverage: PASS — 6 timed sections; largest gap 0 ms (limit 2000 ms)
Audio-description captions coverage: PASS — 6 timed sections; largest gap 0 ms (limit 2000 ms)
Current approvals: 1/3 required roles

Blocking findings (3):
- Narration, chapter 2: uncovered audio gap before this chapter exceeds policy (observed 200 ms; tolerance or limit 100 ms)
- Accessibility: required release sign-off is still pending
- Audio post: sign-off does not cover the current package version (recorded version 2.4.0; release version 2.4.1)
```

The narration and audio-description chapter boundaries are within the
250-millisecond synchronization tolerance. Narration chapter 2 starts 200 ms
after chapter 1 ends, exceeding the stricter 100-millisecond continuity
allowance. Transcript and audio-description captions each contain six
nonempty, ordered time sections spanning the full 90 seconds with no gaps.
The editorial approval is current; the accessibility review is pending; the
audio-post approval covers version 2.4.0 instead of the package's 2.4.1.

## Audit flow

1. `model.sai` defines release metadata, chapter audio parts, timed text
   sections, approval records, findings, and the report.
2. `fixtures.sai` provides three chapter windows, six required audio parts, a
   transcript, audio-description captions, and three review records.
3. `policy.sai` defines the 250 ms sync tolerance, 100 ms audio-gap limit,
   2,000 ms text-gap limit, and fixed six-section text fixture.
4. `audit.sai` checks required track pairs, locale and version identity,
   chapter boundaries, narration and description continuity, text source and
   coverage, and current approvals. Any finding holds the package.
5. `report.sai` renders the decision, coverage summaries, approval count, and
   human-readable findings. `lib.sai` exposes `sample_audit()` and
   `sample_report()` with source-level assertions for the fixture outcomes.

## Limits

This is a bounded in-memory review of one locale and three ordered chapters.
Track time ranges, text cues, the rights flag, and approval records are
trusted inputs. The sample does not open or decode audio, parse VTT or other
caption formats, compare spoken words with a transcript, judge the quality of
audio description, verify legal rights, authenticate reviewers, or send a
release to a distribution service. It assumes the chapter manifest itself is
correct and ordered. Text gaps up to two seconds and audio gaps up to 100 ms
are accepted because those are the fixture's teaching thresholds, not
universal accessibility requirements.

## Review required

- The required narration and audio-description track types, the three
  sign-off roles, and the exact-version approval rule are sample policy
  choices. A publisher should adapt them to the storefront, territory,
  content format, and accessibility commitments for each release.
- The 250 ms synchronization tolerance and 100 ms audio continuity limit may
  not fit every mastering workflow. Review them with audio engineering and
  accessibility specialists before using similar checks on real deliveries.
- The two-second text-gap allowance is a coarse coverage heuristic. Real
  captions and transcripts must be checked against speech, meaningful sound,
  reading speed, speaker identity, and platform-specific timing rules.
- The fixture treats a checked rights flag and approval entry as evidence
  supplied by a trusted upstream process. Neither is independently verified
  here.

## Package and compiler status

The package targets `wasm32-core` and uses the `alloc` capability only to
build the human-readable report string. It has no network, filesystem, or
persistent-state effects. The example uses fixed arrays, records, enums,
module imports, loops, and structured findings.

`sai fmt --check sample/accessible-audio-release-auditor` passes with the
current formatter. The `sai check --offline --target wasm32-core` command is
blocked by the compiler's unimplemented Stage 0 check and `@derive` feature
gates. `sai test --offline` is unavailable at the unimplemented Stage 3 test
gate. Semantic checking and test execution are therefore not verified. The
expected report above is manually reconciled with the fixture; embedded SAI
assertions are source examples and have not been executed.
