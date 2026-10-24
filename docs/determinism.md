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
  environment (round to nearest, no flush-to-zero or denormals-are-zero,
  every exception masked), which is also what Rust assumes. x87-only targets
  (32-bit x86 without SSE2) are not supported. See "Floating-point
  environment" below for how this is enforced.
- The compiler never constant-folds `^` (a folded literal would be free where
  a runtime `^` is charged), so bytecode carries no precomputed `^` results.

## Floating-point environment

A host such as a game engine may set flush-to-zero, denormals-are-zero, a
directed rounding mode or exception traps on its threads. Every subnormal or
inexact result would then differ from other hosts with no error, so dellingr
refuses to compute in such an environment instead.

- On x86_64 and aarch64, dellingr reads the calling thread's control register
  (`MXCSR`, `FPCR`) and refuses unsupported settings with
  `ErrorKind::UnsupportedFloatEnvironment` (`SaveError::` and
  `LoadError::UnsupportedFloatEnvironment` for snapshots). The error carries
  the register value and decodes it. Reading changes nothing: dellingr never
  resets the environment, which would be undefined behaviour in Rust.
- Checked: every public fallible `State` method made by host code,
  `Program::compile`, `save_state`, `load_state` on entry and again after
  its setup closure, and every Rust callback's return, where a callback that
  returns `Ok` with the environment changed fails with this error instead,
  so Lua never resumes under it. A host function's own calls into the State
  are checked; a standard-library function's are not, since it only runs
  after a check that followed the last host code. Creating a State is not
  checked: it computes nothing that depends on the environment, and a State
  may be created on one thread and run on another.
- Not checked: the infallible observations (`to_boolean`, `raw_equal`,
  `typ`, `get_top`, ...). `raw_equal` on a subnormal under denormals-are-zero
  answers as any host comparison would.
- The refusal is charged nothing and does not poison the State. Refused at
  entry, the operation did no work and is not reported to `on_error`; raised
  at a callback's return, it is that callback's error, unwinds like any
  other and is reported once. A call that failed partway keeps what it did
  before the failure, so restoring the environment and retrying is not a
  replay-safe recovery; reloading a snapshot is.
- Detection is best-effort: a non-default environment violates what the
  compiler assumes, so a floating-point instruction may be moved ahead of a
  check, and an environment with exception traps enabled may fault rather
  than be refused. Refusal paths publish no arithmetic result of the refused
  operation. Values a host computes in a bad environment and restores the
  environment before handing over cannot be detected.
- Other targets (including 32-bit x86, whose x87 control word is not read)
  keep the requirement without runtime enforcement.

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
functions render as `function: 0x<id>`. Identities rendered during load setup
are merged with the saved ones: a value the save records takes its saved
identity, and a setup that does not replay the saving State's initialization
can print ids that do not hold after the load (see `save-state.md`).

Rust functions are compared by their entry in the State's function table,
never by code address: each `add_rust_fn` or `register_rust_fn` call creates a
new function, and every push of its handle is the same value. `f == g`, table
keys and rendered identities of host functions therefore do not depend on the
build, even when the optimizer or linker merges identical functions. See
`compatibility.md` and `save-state.md`.

Identities are never reused, short of a load whose setup differs from the
saving State's initialization. Strings are interned and also get a `%p`
identity, so a string that is dropped and later rebuilt with the same bytes
keeps its id only if no collection ran in between. The metamethod event names
(`__index`, `__newindex`, `__call`, `__len`, `__tostring`, `__metatable`) are
never collected, so their ids are stable for the State's lifetime and across a
snapshot round trip.

## Inline caches

Lookup sites cache what they found, but the caches are an acceleration
only: whether an instruction succeeds, what it returns and costs, how much
value stack it needs and how much metamethod depth it uses never depend on
which values the site saw before. Snapshots do not save caches, so a State
resumed from a snapshot starts them cold and continues exactly as the State
that saved it would have.

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
