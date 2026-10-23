# Determinism

The same script and inputs produce the same result and the same cost on a
given build. The library reads no host state: no environment, clock, process
arguments, unseeded RNG or randomized hashing. The CLI and the bench harness
are hosts and are exempt.

## Math

- `math.random` draws from the in-crate `VmRng` (SplitMix64), seeded by
  `set_rng_seed` (default `VmRng::DEFAULT_SEED`, which is 0). A fixed seed gives
  the same stream on every platform. Rust ownership keeps the stream single
  owner within each state and prevents accidental generator forks.
- Basic arithmetic, comparisons, `sqrt` and `math.random` are bit-stable
  across platforms.
- `math.fmod` uses Rust's binary64 remainder operation; Lua `%` uses the same
  operation and then applies its floor-sign correction. The underlying
  remainder is exact for binary64 operands on supported hosts, though a build
  may lower it to the platform's `fmod`; it is not an in-crate CORE-MATH
  function.
- A singleton integer range, as in `math.random(1)` or `math.random(m, m)`,
  returns its only value without advancing the RNG stream. This is part of the
  replay-visible draw sequence.
- `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log` (with or
  without a base) and the `^` operator are computed in-crate, never by the
  platform libm, and give the same bits on every supported host. They are
  ports of the CORE-MATH correctly rounded implementations; see
  `reference/deterministic-math.md` for how the guarantee is built.
- They are also correctly rounded, with two qualifications. `math.log(x, b)`
  for a base other than 10 is the quotient of two correctly rounded natural
  logarithms, rounded again. `^`, `atan2` and two-argument `atan` end in a final
  high-precision phase that could, in principle, fail to certify a rounding;
  no input is known to reach that point, and the result is then the rounding
  of the high-precision approximation: still the same on every host, but
  possibly one unit in the last place off. Correcting such a case later would
  be a replay-visible change.
- The supported hosts are those that evaluate binary64 arithmetic exactly as
  IEEE 754 specifies, with gradual underflow and the default floating-point
  environment (round to nearest, no flush-to-zero), which is also what Rust
  assumes. x87-only targets (32-bit x86 without SSE2) are not supported.
- The compiler never constant-folds `^` (a folded literal would be free where
  a runtime `^` is charged), so bytecode carries no precomputed `^` results.

## Number rendering

- `tostring` and `string.format` render numbers with `%.14g`, independent of
  platform: `0.3`, `1e+15`, `-0`.
- NaN renders unsigned (`nan`) everywhere. No script can observe the sign or
  payload of a NaN.
- Snapshots write every NaN as one canonical NaN. See `save-state.md`.

## `pairs` order

- The array part is visited first, in ascending key order. Appending `n + 1`
  also moves any already-present consecutive keys `n + 2`, `n + 3`, and so on
  from the hash part into the array part. Those migrated keys are therefore
  visited before remaining hash entries, even if they were inserted earlier.
- Other keys still in the hash part follow in insertion order.
- `n` never shrinks, even when the low keys are cleared. Snapshots preserve
  it, so a loaded state continues with the same order and `#` results.
- Cleared hash keys remain valid `next` controls until insertion compacts
  their slots. Collection keeps those keys alive, and snapshots preserve the
  dead slots and storage kind so traversal can resume after load.

## Object identity in output

`tostring` and `%p` of tables, closures and Rust functions render a state-local
identity, not an address. It is stable across a snapshot round trip. Rust
functions render as `function: 0x<id>`.

Rust functions are compared by code address, so `f == g` on host functions
depends on the build: identical-code folding can make two distinct functions
equal. Addresses themselves never reach output or ordering. See
`compatibility.md` and `save-state.md`.

Identities are never reused. Strings are interned and also get a `%p`
identity, so a string that is dropped and later rebuilt with the same bytes
keeps its id only if no collection ran in between. The metamethod event names
(`__index`, `__newindex`, `__call`, `__len`, `__tostring`, `__metatable`) are
never collected, so their ids are stable for the State's lifetime and across a
snapshot round trip.

## Garbage collection timing

Collections fire on allocation-count thresholds, so their timing is
deterministic for a given build. It is not a cross-version contract: a release
may move allocation points or collection thresholds, which can change `%p` ids
of rebuilt strings and `State::object_count()`. Such changes are called out in
the changelog, like other replay-visible changes.

## Error labels

Function labels in argument errors are diagnostic text, not stable
identifiers, and are not a cross-build log format.

- Standard-library validators that use their named helpers supply explicit
  names. These do not vary.
- Generic Rust callbacks checked with `State::check_any` or
  `State::check_type` get a label from the current global bindings or, with
  the `snapshot` feature, from the registered function id.
- The label can therefore differ between feature sets or builds, and when
  globals are rebound.

Match on `ErrorKind`, not on `Display` text. See
`reference/error-reporting.md` for the naming rules.
