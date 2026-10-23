# Open Source License Review

Open Source License Review checks a fixed component inventory against a
fictional distribution policy. For each component, it reports whether the
supplied evidence is complete and whether the sample policy clears it, blocks
it, or requires a person to review it.

The license labels and policy classes are teaching fixtures. They are not
legal classifications or legal advice, and a “cleared” result means only that
the supplied record passed this example's rules.

## Hand-checked example result

`review.example_report()` returns nine findings in fixture order:

| Component | Sample class | Evidence | Result |
|---|---|---|---|
| image-optimizer | Notice-based allowed | Complete | Cleared by sample policy |
| templating-engine | Review required | Complete | Manual review required |
| copyleft-plugin | Restricted by sample policy | Complete | Blocked by sample policy |
| font-bundle | Notice-based allowed | Notice file missing | Missing notice |
| codec-wrapper | Notice-based allowed | Conflicting metadata | Conflicting metadata |
| legacy-connector | Restricted by sample policy | Complete; exception approved | Exception approved |
| build-helper | Unclassified | Complete | Manual review required |
| theme-pack | Restricted by sample policy | License text missing; exception marked approved | Missing license text |
| experimental-sdk | Notice-based allowed | Complete; exception marked approved | Exception scope mismatch |

The summary is: 1 cleared, 2 requiring manual review, 1 blocked, 1 missing
license text, 1 missing a notice file, 1 with conflicting metadata, 1 approved
exception, and 1 exception with an incompatible scope. The sample policy does
not consider this inventory ready for distribution because unresolved findings
remain.

## Review flow

1. `fixtures.sai` supplies nine component records with a human-readable label,
   a preassigned sample policy class, evidence state, and optional exception
   approval.
2. `policy.sai` checks evidence before applying the sample class rules.
   Missing or conflicting evidence cannot be overridden by an exception.
3. `report.sai` preserves fixture order, creates one result per component, and
   counts each outcome. An exception approval applies only to a restricted
   sample class; an approval on another class is reported as a scope mismatch.
4. `lib.sai` exposes the example report, its readiness result, and fixed
   regression assertions.

All ordering is deterministic: each finding stays in the same position as its
input component. No names are parsed, sorted, fetched, or resolved against a
license database.

## Limits

This is a bounded, in-memory example with nine fixed entries. It does not
parse package manifests, SPDX expressions, license texts, source files, or
dependency graphs; it does not validate whether an evidence file is genuine
or sufficient; and it does not make legal conclusions. Policy classes and
approval flags are trusted fixture inputs. A production review would need
jurisdiction-specific policy, verified evidence, dependency context, and
qualified review.

## Review required

- The classes `NoticeBasedAllowed`, `ReviewRequired`,
  `RestrictedByDemoPolicy`, and `Unclassified` are invented policy labels.
  They deliberately avoid asserting that a named real license is always
  compatible, incompatible, or subject to a particular notice obligation.
- The fixture treats missing license text and missing notice evidence as
  blockers even when an exception is marked approved. This prioritizes
  evidence completeness over policy approval; a real organization must define
  the scope and ordering of its own exception process.
- The sample accepts an explicitly approved exception for a restricted class
  only after evidence is complete. The fixture includes an approval flag for
  every component; a true flag attached to another class is flagged for scope
  review instead of silently ignored.
- `ready_under_sample_policy` is only the result of these invented rules. It
  must not be used to approve an actual release.

## Package and compiler status

The manifest targets `wasm32-core`; the package has no external dependencies
or runtime capabilities. The source illustrates modules, records, enums,
fixed-size arrays, `match`, loops, derived traits, and SAI test declarations.
The current reference compiler's Stage 0 `sai check` and Stage 3 `sai test`
are not implemented, and `@derive` is a Stage 2 feature. The expected results
above are hand-checked; no successful type-check, test, build, or execution is
claimed.
