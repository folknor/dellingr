# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #12 and #33: rewrite the Lua pattern matcher

Two matcher bugs that cannot be patched in place, because both are consequences
of the module's shape rather than local mistakes. `notes/optimizations.md` #4
already specifies the rewrite that fixes them structurally; this loop executes
it.

Both targets are **verified by execution**, and Lua 5.2 and 5.4 agree on every
case - these are universal conformance bugs, not 5.4 tightenings.

| case | dellingr | Lua 5.2 | Lua 5.4 |
|---|---|---|---|
| `s = ("a"):rep(250); s:find("a%d")` | error `pattern too complex` | `nil` | `nil` |
| `("abcd"):gsub("%f[%w]%w", "X")` | `XXXX  4` | `Xbcd  1` | `Xbcd  1` |
| `("ab"):find("%f[%a]%a", 2)` | `2  2` | `nil` | `nil` |

### #12 (High) - matchdepth is consumed by tail calls and never reset

`patt_match` decrements `matchdepth` on entry (`luapat.rs:349`) and restores it
only on the two fallthrough exits (427, 475). Six transitions instead do a Rust
tail call and never restore:

- `luapat.rs:389` - `%b` continuation
- `411` - `%f` continuation
- `419` - backreference continuation
- `440` - `patt_default_match` accept-empty for `*` / `?` / `-`
- `453` - the `?` else-branch
- `471` - item with no suffix

Reference C uses `goto init` for exactly these, so continuation is depth-free
there. Two distinct failure modes follow:

1. **Pattern length.** A chain of N non-suffixed items costs N depth, so any
   pattern with more than ~198 sequential items fails. Reference matches
   arbitrarily long patterns.
2. **Leak across the scan loop.** `str_match`'s loop (`luapat.rs:674-687`)
   never resets `matchdepth` or `level` between anchor positions, so every
   failed attempt that partially matched leaks depth equal to its tail-chain
   length. 5.2 asserts `matchdepth == MAXCCALLS` before every attempt.

Mode 2 is why the repro above dies on its *first* `find` rather than after 200:
250 failed anchor attempts against a 250-byte subject leak one level each.

`src/patterns/mod.rs:322` (`runtime_match_errors_are_not_swallowed`, 201
literal `a`s) asserts the divergence as expected behaviour and must be replaced,
not deleted - the property it names is worth keeping, so it needs a subject and
pattern that still legitimately exceed the depth limit after the fix.

### #33 (Medium) - `%f` loses left context because callers re-slice the subject

The matcher treats slice-start as string-start (`previous = '\0'`,
`luapat.rs:401-405`), but every caller hands it a suffix rather than the whole
subject with an offset:

- `string.rs:316` - `find` with `init`
- `409` - `match` with `init`
- `565` - the `gsub` loop
- `702` - `gmatch_iter`
- `288` - the plain-find fast path

Reference `prepstate` keeps `src_init` at the true beginning of the subject even
when matching from `init` or resuming mid-string. Every one of the ~14 `base` /
`init` / `pos` capture-offset arguments threaded through `push_capture_value`,
`push_captures`, `append_capture_bytes` and the gsub replacement helpers exists
only to compensate for that re-slicing.

### Why a rewrite rather than two patches

`notes/optimizations.md` #4 is the standing plan and it covers both:

- Loop-based tail transitions (C's `goto init`) instead of Rust tail calls
  fix #12's depth semantics and remove real per-item call overhead.
- An `init` offset parameter, with the whole subject passed once, fixes #33 and
  deletes the `base` arithmetic across `string.rs`.
- Safe `usize` offsets into `&[u8]` instead of the raw-pointer `CPtr` layer
  eliminate the `unsafe` derefs and the empty-pattern UB class.
- Precompiling the pattern into an item list removes `classend`'s re-scan of
  class bytes at every subject position tried by the scan loop (today O(pattern)
  per position) and folds the validator and compiler into one pass with one
  authoritative capture count.

The cost hook for #16 lands naturally in this shape. **Do not charge anything
in this loop** - #16 is a deliberately staged breaking cost-model change. Leave
the structure ready and nothing more.

### Constraints

- The public surface is `LuaPattern` in `src/patterns/mod.rs`
  (`from_bytes_try`, `matches_bytes`, `range`, `capture`, `num_matches`) plus
  `LuaCapture` and the `PatternError` taxonomy in `patterns/errors.rs`. Error
  variants and their `Display` strings are asserted by tests in `mod.rs` and by
  `tests/gsub_errors.rs`; they are contract, keep them exact.
- `LUA_MAXMATCHES` is 32 and the 32-capture limit stays.
- Determinism is a product requirement. No iteration-order or entropy changes.
- `unwrap_used` is denied outside `#[cfg(test)]`; `HashMap`/`HashSet` banned.
- The whole existing `mod.rs` test suite must keep passing unchanged, with the
  single exception of `runtime_match_errors_are_not_swallowed`.

---

## Agreed implementation plan

The orchestrator and the deep reviewer verified both findings independently and
converged on the same design, including the same depth boundary derived
separately against reference. What follows is settled, not a menu.

### Depth model

Count **active invocations of the core matcher, including the initial one**.
Permit 200 concurrently active; reject the 201st. Pass depth **by value** as an
argument rather than mutating a shared remaining-budget field:

```rust
if depth >= MAXCCALLS {
    return Err(PatternError::MatchDepthExceeded);
}
```

Recursive calls receive `depth + 1`; loop continuations keep `depth` unchanged.
Each anchor attempt starts at `depth = 0`. This also corrects the current
off-by-one, where the guard fires when the decrement reaches zero.

Only these edges recurse and therefore count:

- `start_capture`'s call back into the matcher (`luapat.rs:302`)
- `end_capture`'s (`313`)
- every continuation attempt from `max_expand` (`271`) and `min_expand` (`283`)
- the greedy speculative branch of `?` (`449`)

The helpers themselves consume no level - only their calls back in do. The `?`
fallback at `453` is a tail continuation and must **not** consume a level.

Everything else becomes a loop iteration, mirroring C's `goto init`: the `%b`,
`%f` and backreference continuations (`389`, `411`, `419`), the accept-empty
path (`440`), and the no-suffix item (`471`).

**Verified boundary, both 5.2 and 5.4:** `("a?"):rep(199)` over 199 `a`s
matches; `("a?"):rep(200)` over 200 `a`s raises `pattern too complex`; 201
sequential *literal* items match fine. The rewrite must reproduce this exactly.

### Capture state per attempt

`level` does **not** leak today - `start_capture` decrements on failure
(`303`) and `end_capture` restores `Unfinished` (`316`), and an error aborts
`str_match` outright, so no later attempt observes partial unwind. Confirmed
behaviourally: `("aac"):find("(a)c")` gives `2, 3, "a"` in dellingr and both
references.

Reset it anyway. Every anchor attempt starts with fresh capture state and
`depth = 0`, so the invariant is structural rather than contingent on every
unwind path being correct.

### Whole subject plus `init`

An `init` offset is sufficient; no separate end offset is needed. The suffix
slices lose only the **left** boundary - `src_end` is already derived from the
suffix pointer plus its length (`luapat.rs:664`), which is why `$` and the
frontier end sentinel already behave correctly at resumed positions. Confirmed
identical across dellingr and both references:

- `("ab"):find("$", 2)` -> `3, 2`
- `("ab"):find("%f[%z]", 2)` -> `3, 2`
- `("ab"):gmatch(".%f[%z]")` -> `"b"`
- `("ab"):gsub("%f[%z]", "X")` -> `"abX", 1`

New entry point is conceptually `matches_bytes_from(subject, init)` with
`src_init = 0`, `src_end = subject.len()`, scanning from `init`.
`matches_bytes(subject)` stays as the zero-offset wrapper.

### Offset cleanup in `string.rs`

Delete the `base` parameter outright from `push_capture_value` (63),
`push_captures` (70), `append_capture_bytes` (83), `append_string_replacement`
(92) and `append_gsub_replacement` (139). Pass the whole subject;
`LuaCapture::Bytes` indexes it directly and `LuaCapture::Position(offset)` needs
only `offset + 1`.

Offsets that legitimately remain: `init`/`pos` as the scan-start argument and
gmatch iterator state; the `+1` zero-to-one-based conversion; the one-byte
advance after an empty match (580, 710); `gsub`'s `pos` for copying unmatched
text; and the plain-search conversion at 288, since `find_subslice` still
reports suffix-relative results.

### Compiled representation

One pass producing an ordered program plus precompiled byte classes:

```rust
enum Item {
    Atom { atom: Atom, repeat: Repeat },
    Balance { open: u8, close: u8 },
    Frontier { class: ClassId },
    Backref { slot: u8 },
    CaptureStart { slot: u8 },
    CaptureEnd { slot: u8 },
    PositionCapture { slot: u8 },
    EndAnchor,
}

enum Atom { Literal(u8), Any, Class(ClassId) }

enum Repeat { One, Optional, ZeroOrMoreGreedy, OneOrMoreGreedy, ZeroOrMoreMinimal }
```

Each class is a deterministic 256-bit set (`[u64; 4]`) in a `Vec<ByteClass>`
referenced by `ClassId`, so bracket classes, complements, ranges, `%a`-style
classes, frontier classes and `%z` all test in O(1) per subject byte with no
reparsing. This is what removes `classend`'s per-position rescan.

The compiler keeps a fixed 32-entry capture stack, assigns slots, resolves
`CaptureEnd` and backreferences, records the one authoritative capture count,
and emits the existing errors in left-to-right order. Leading `^` is pattern
metadata, not an item.

### Error timing is contract - do not shift it

- Compilation happens in `from_bytes_try` (`mod.rs:24`), where `str_check`
  already runs. Every validation error keeps firing there.
- `matches_bytes` retains only data-dependent errors: `MatchDepthExceeded`,
  plus the two backreference cases below.
- Preserve every `PatternError` variant and its exact `Display` string
  (`errors.rs:33`). `tests/gsub_errors.rs` and the `mod.rs` suite assert them.
- A backreference to an *unfinished* capture stays a match-time
  `UnfinishedCapture`; a backreference to a *position* capture stays a plain
  non-match, not an error (`mod.rs:279`).
- Do not move validation ahead of the past-end checks in `find`/`match`
  (`string.rs:312`, `401`), nor ahead of `gsub`'s zero-replacement early return
  (`489`).
- `gmatch` compiles lazily when the iterator first runs (`703`). Keep that.
  Caching an owned compiled matcher in iterator state is a snapshot/state-design
  change and is out of scope.

### Scope limits

- **Charge nothing.** Leave one centralized matcher-step site suitable for the
  future #16 cost hook, but add no meter and no `consume_cost`. #16 is a staged
  breaking cost-model change and is not part of this loop.
- `LUA_MAXCAPTURES` stays 32; `LUA_MAXMATCHES` stays 33.
- The entire `mod.rs` test suite keeps passing unchanged except for the body of
  `runtime_match_errors_are_not_swallowed`.

### Tests

Add before rewriting: the #12 no-leak repro and the 199/200 boundary; every #33
entry and resumption case; absolute byte and position captures; resumed `$` and
`%f[%z]`; and the deferred-error timing cases above. Replace only the body of
`runtime_match_errors_are_not_swallowed`, keeping its name and intent.

A new `examples/` script covering the #33 cases needs **no `DIFF` marker** -
5.2 and 5.4 agree with the fixed behaviour.

Read `src/patterns/luapat.rs`, `src/patterns/mod.rs`, `src/patterns/errors.rs`,
and the matcher call sites in `src/lua_std/string.rs`.
