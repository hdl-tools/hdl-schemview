# ADR 0011 — Validation policy for the rkyv load cache

- **Status:** Proposed — the recommendation is option 3; not yet decided
- **Date:** 2026-08-16
- **Deciders:** project maintainers
- **Relates to:** issue #237 (blocked on this), issue #155, issue #304;
  amends [ADR 0003](0003-storage-backend-for-parse-scalability.md)'s Phase A as-built

## Context

The #21 load cache landed as ADR 0003's **Option A**: a hit mmaps the archive,
**bytecheck-validates** it, then deserializes into an owned `Document`. Validation is not a
rounding error at scale.

Measured at 1M nodes (`docs/benchmarking.md` §Findings, 2026-07-26):

| step | cost |
| --- | --: |
| `access_cache_unchecked` (mmap, no validation) | **0.29 ms** |
| `access_cache_checked` (mmap + bytecheck) | **294 ms** |
| full `cache_hit` (＋ deserialize ＋ index build) | 1414 ms |

Validation is **~294 ms, ~21% of warm load, and ~1000× the cost of not doing it.**

This matters now because #237 proposes reading the archive as `&ArchivedDocument` instead of
deserializing. That removes the deserialize term but **keeps validation** — so validation
becomes the floor under every future load-time improvement, and #237 cannot be scoped, let
alone accepted, until this is settled. #237 says as much: *"Settle this before
implementing."*

Two facts frame the choice:

1. **The archive is a derived cache, never a source of truth.** `hierarchy.json` is golden;
   the archive can always be discarded and rebuilt. Nothing is lost by rejecting one.
2. **The failure mode is not theoretical.** The cache is a file on disk under
   `.schemview_data/`. It can be truncated by a crash mid-write, by a full disk, by a
   killed process, or by a network filesystem doing something creative. `write_bytes_atomic`
   (temp file + rename) makes *our* writes atomic, but it does not defend against
   post-write corruption, a partially-synced file, or another tool touching the directory.

`rkyv::access_unchecked` on a corrupt archive is **undefined behaviour** — not a wrong
answer, not a panic. That is the whole of the decision.

## Decision

**Proposed: option 3 — a cheap integrity gate plus unchecked access, with the JSON as
fallback.** Not yet ratified; the alternatives are recorded below with why they lose.

Store a checksum of the archive bytes in the existing header (which already carries
`rkyv_format_version`, `schema_version`, `src_len`, `src_mtime_ns`). On load: verify the
checksum over the mapped bytes, and only then use unchecked access. Any mismatch is treated
exactly as a stale cache is today — return `None`, fall back to JSON, rewrite the archive.

The header check stays where it is, before the payload is touched.

### Why this shape

- A modern checksum over ~380 MB is **memory-bandwidth bound, not structure-bound**. It is
  one linear pass with no pointer chasing, where bytecheck walks the object graph validating
  every relative pointer and slice bound.
- It defends against the failure that actually happens — **truncation and bit-rot** — which
  is also the only failure that makes unchecked access unsafe.
- It preserves the existing contract exactly: a bad cache is a miss, and a miss is already
  a fully supported, tested path.
- It leaves `RKYV_FORMAT_VERSION` doing its current job. A checksum field is a header
  change, so it needs one bump; after that the policy is stable.

**This must be measured before ratifying.** The claim "checksum ≪ bytecheck" is an
expectation, not a result, and this project's recent history is unkind to unmeasured
expectations — #238 set out to archive an index and found the archiving worth 0.8%. If a
checksum lands within a small factor of bytecheck's 294 ms, option 1 wins on simplicity and
this ADR should be closed as rejected.

## Alternatives considered

### 1. Keep bytecheck (status quo)

Validate every load, pay 294 ms at 1M.

**For:** memory-safe by construction, zero new code, no new failure mode to reason about.
**Against:** it is the floor under #237 and every later load improvement, and it re-verifies
structure we ourselves wrote moments earlier, on every launch, forever.

Not unreasonable — if the checksum measurement disappoints, this is the answer, and 294 ms
buys real safety.

### 2. Unchecked access, unconditionally

Trust the archive because we wrote it.

**Against:** rejected. The archive outlives the process that wrote it, on storage we do not
control, and the penalty for being wrong is UB rather than a bad answer. "We wrote it" is
not a property that survives the file sitting on disk between launches. The 0.29 ms number
is a *floor to measure against*, not a shipping proposal.

### 3. Checksum gate + unchecked access *(proposed)*

As above.

**Against, honestly:** a checksum proves the bytes are the bytes we wrote; it does not prove
they are *well-formed rkyv*. Those coincide only because the writer is trusted and the
format version is checked. That reasoning holds today; it would need revisiting if archives
were ever shared between machines or produced by anything but this binary — which is
exactly the sort of thing to state now rather than discover later.

### 4. Validate lazily, per subtree, on first touch

Amortize validation over what is actually read.

**Against:** rkyv's validation is whole-archive, so this means hand-rolling per-region
validation and tracking what has been validated — substantial machinery, and a partial-touch
model that interacts badly with the index build, which reads essentially everything anyway.
Revisit only if loads ever become genuinely partial (i.e. alongside #22's Phase B).

## Consequences

- **#237 becomes scopeable.** Under option 3 its budget is validation *and* deserialize
  (~644 ms at 1M) rather than deserialize alone (~350 ms) — nearly double the win, and the
  difference between "large refactor for the smallest term" and a defensible one.
- **One `RKYV_FORMAT_VERSION` bump** to add the checksum field; every existing archive
  misses once and is rewritten. Harmless — the cache is derived.
- **A new invariant to hold:** the unchecked path may only be reached after the checksum
  passes. That is a small, testable surface, and it should have a test that a deliberately
  corrupted archive falls back to JSON rather than being read.
- **`access_cache_checked` / `access_cache_unchecked` keep their role** as measurement
  hooks (#236) and gain a third sibling to price the checksum.
- If the measurement kills option 3, this ADR flips to **Rejected**, ADR 0003's Phase A
  as-built stands unchanged, and #237's budget stays at the deserialize term alone — which
  on the numbers in #304 makes #237 a lower priority than restructuring `path_index`.

## Open questions

1. **Which checksum?** Something fast and non-cryptographic (xxh3, crc32c with hardware
   support) — integrity against corruption, not against an adversary. If an adversary can
   write to `.schemview_data/` they can write to `hierarchy.json` too.
2. **Checksum the whole archive, or the payload after the header?** Whole file is simpler;
   header-excluded avoids a chicken-and-egg on the field itself.
3. **Is the 380 MB pass worth doing at all at small scale?** At 665 nodes the whole warm
   load is 3.7 ms and none of this matters. A size threshold would be a premature
   optimization, but worth confirming the checksum does not regress small designs.
