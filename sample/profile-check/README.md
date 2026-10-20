# Profile Check

Profile Check parses a small `key = value` profile format into typed fields,
then validates the parsed values before reporting an accepted profile. It
handles one configuration error at a time with a `Result<Profile, ConfigError>`
so malformed input can be reported without panicking.

```text
profile-check valid

Valid example
Accepted profile for Rin Tanaka
  email: rin@example.org
  age: 29
  visibility: public
  daily goal: 4 items

profile-check invalid

Validation example
Rejected profile: age 12 is outside the supported range (13 to 120)
```

## Commands

- `valid` parses and accepts the built-in example profile.
- `invalid` parses a profile whose age fails semantic validation.
- `malformed` shows the numeric parsing error for a non-numeric age.
- `unknown-key` reports a field the format does not define.

The parser trims whitespace around keys and values, permits full-line `#`
comments, requires each known field exactly once, and rejects unknown fields.
The validator requires a non-empty name, a plausible email address, an age
from 13 through 120, and a daily goal from 1 through 12. Email validation is
deliberately small: it checks for a non-empty local part and a dotted domain,
not full RFC conformance. Each command uses a built-in fixture; the example
does not read a configuration file or persist changes.

## Package and compiler status

The package targets `wasm32-wasi` and uses the `sai:process` command-line
entry-point shape from the other terminal samples. It is an illustrative
project, not a build guarantee: the current compiler does not yet implement
the complete package, process, command-line, multi-module, iterator, and
formatting pipeline used here. The command dispatcher assumes the host
supplies the program name as `argv[0]`. The parsing and validation functions
are designed to return borrowed views into their input text, following the
`borrows` contract in the SAI specification.
