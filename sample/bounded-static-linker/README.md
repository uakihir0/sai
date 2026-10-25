# Bounded Static Linker

This sample links tiny, already-parsed object manifests into a deterministic
address map. It demonstrates section layout, symbol binding, and relocation
validation without reading object files or modifying machine code. If any
input, symbol, or relocation is invalid, the result contains an issue and no
`LinkedImage`; the linker never publishes a partially linked image.

## Successful fixture

The main fixture has three `.text` sections in this order:

| Object | `.text` size | Alignment | Base |
| --- | ---: | ---: | ---: |
| `app` | 12 | 4 | 0 |
| `helper` | 16 | 4 | 12 |
| `fallback` | 4 | 4 | 28 |

`app` has an `Abs32` relocation at offset 4 for `normalize`. Both `helper` and
`fallback` define that name, but `helper` is strong and `fallback` is weak.
The strong definition wins: its offset 8 resolves to address `12 + 8 = 20`,
which is the reported relocation value. The total laid-out size is 32 bytes.

## Deterministic rules

- Inputs contain at most three objects, one `.text` section per object, up to
  four symbol entries and two relocations per object.
- Objects are laid out in input order. Each section base is rounded up to its
  power-of-two alignment using checked `U32` arithmetic.
- A strong definition wins over weak definitions. If no strong definition
  exists, the first weak definition wins by object order and then symbol-table
  order. Two strong definitions with the same name reject the link.
- A local symbol is visible only within its defining object. It cannot satisfy
  another object's relocation. For a request from that same object, the first
  matching local definition wins before global weak/strong lookup.
- `Abs32` is the only relocation kind. Its reported value is the checked sum
  of the target section base, symbol offset, and addend.
- A four-byte patch is in bounds when
  `patch_offset + 4 <= text_size`. A patch ending exactly at the section
  boundary is valid.
- All objects and relocations must validate before a `LinkedImage` is
  returned. Any failure produces no image and no partially applied
  relocations.

## Fixture outcomes

The source fixtures and regression assertions cover these hand-checked
outcomes:

| Scenario | Expected outcome |
| --- | --- |
| Strong `normalize` beside a weak fallback | Link succeeds; bases are `0`, `12`, `28`; the relocation value is `20`. |
| Two same-name weak entries in one object, plus a later object's weak entry | Link succeeds; the first definition by object and symbol order is selected. |
| A five-byte section followed by an eight-byte-aligned section | Link succeeds; the second base is `8` and the total size is `12`. |
| Two strong definitions of one name | Reject with `DuplicateStrongDefinition`; no image is returned. |
| Cross-object relocation to a local-only symbol | Reject with `UnresolvedSymbol`; no image is returned. |
| Same-object local `secret` at offset 2 beside another object's strong `secret` | Link succeeds and records the local definition with value `2`, before global lookup. |
| Four-byte patch ends exactly at `.text` size | Link succeeds. |
| Section is shorter than four bytes, or a patch extends past `.text` size | Reject with `RelocationOutsideSection`; no image is returned. |
| Alignment is zero or not a power of two | Reject with `InvalidSectionAlignment`; no image is returned. |
| Section placement exceeds the `U32` address space | Reject with `AddressOverflow`; no image is returned. |
| Resolved address plus relocation addend exceeds `U32` | Reject with `AddressOverflow`; no image is returned. |
| A later relocation cannot resolve after an earlier one succeeds | Reject the link and return no image or partial fixup list. |

The successful and rejected outcomes describe the deterministic fixture
contract, not compiler-generated output.

## Scope and project layout

This is a typed-manifest exercise, not a production linker. The input records
are already parsed. The result reports addresses and relocation values; it
does not patch bytes or load an executable.

- `src/model.sai` defines bounded objects, symbols, relocations, and reports.
- `src/layout.sai` validates object metadata and computes aligned section
  bases. Direct callers also receive a typed error for malformed input.
- `src/symbols.sai` applies local visibility and deterministic weak/strong
  selection; invalid manifests and out-of-range requester indexes return no
  selection.
- `src/linker.sai` checks relocation bounds and arithmetic before returning
  the complete image manifest.
- `src/fixtures.sai` and `src/lib.sai` define the examples and regression
  assertions.

It does not parse ELF or Mach-O, model multiple sections, perform real
machine-code patching, encode target endianness, or support relocation kinds
other than `Abs32`. Its capacities and 32-bit address space are intentionally
small and explicit.

## Review required

- **Binding policy decision:** A strong definition wins, and the earliest weak
  entry by object and symbol-table order wins among weak-only duplicates. This
  keeps output reproducible without depending on platform link order. Review
  whether this is the clearest teaching rule; production linkers may differ.
- **Local visibility decision:** A same-object local definition shadows a
  global definition, while a cross-object reference to a local symbol is
  unresolved. The manifest has no richer visibility or symbol namespaces, so
  this rule keeps lookup bounded and explicit.
- **Atomic output decision:** Any invalid input or relocation rejects the full
  link and returns no `LinkedImage`. This prevents callers from consuming a
  partially relocated manifest after one fixup fails.
- **Address model decision:** `Abs32` values use unsigned addends in one
  contiguous `.text` layout. This keeps the exercise independent of instruction
  encoding and target-specific signed relocation rules.
- **Hostile review:** The first pass found missing malformed-input guards,
  weak-table tie coverage, positive alignment-padding coverage, and local
  precedence coverage. Guards and fixtures now cover each case; final review
  found no remaining actionable findings.

## Formatting and compiler status

When all source modules are present, check formatting from the repository root:

```sh
sai fmt --check sample/bounded-static-linker
```

The current reference compiler has incomplete Stage 0 `sai check` and
`@derive` support, and Stage 3 `sai test` is not implemented. Source-level
regression assertions therefore document intended behavior but cannot yet be
treated as executed verification. Formatting alone does not establish semantic
or runtime correctness.
