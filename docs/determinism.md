# Determinism

The same script and inputs produce the same result and the same cost on a
given build. The library reads no environment, clock, process arguments,
unseeded RNG or randomized hashing. The CLI and the bench harness
are hosts and are exempt.

The library polls two asynchronous host inputs during a run. One is the
`Interrupt` flag (see
`budget.md`, "Stopping a script from the host"). A run that is never
interrupted is unaffected by it. One that is ends at a point that depends on
timing, so it is not a replay; the flag is not saved in snapshots.

The other is the anchor release queue. Dropping an `Anchor` on any thread
queues its release; collections (including automatic collections during Lua
execution) and anchor creation drain the queue. A concurrent drop can therefore
change which collection frees a value retained only by that anchor. This can
change object and string counts, later collection thresholds, and the `%p`
identity of a string rebuilt after collection. For replay, drop anchors on the
State's thread at deterministic points, or finish and synchronize their drops
between runs before resuming the State. Concurrent drops during a run are safe
but their timing is an additional input that a replay must control.
`anchor_count` also reads the queue: it subtracts drops at once, before any
drain, so a concurrent drop changes what it returns at any moment, and a host
callback that hands the count to a script carries that timing into the run.

Draining only while the State is at rest (no invocation in flight) was
considered and declined. It would make a run's releases depend only on drops
ordered before its entry, so a host could synchronize drops against its own
calls rather than against execution. But a host callback that anchors and drops
values inside one long invocation would then keep every dropped value rooted,
and every slot occupied, until that invocation returns, even when every drop is
on the State's thread and already deterministic. Release one at once with
`release_anchor` where the point of release matters.

## Cost

The costing a State was built with is part of what determines a run. Cost
weights decide `cost_used` and, under a budget, where a callback fails, so a
different `CostModel` (or `Costing::Off`) is a replay-visible difference like a
different seed. A replay or stored result must record the
costing along with `COST_MODEL_VERSION`: the version plus the costing
(`CostModel::iter`) is the cost fingerprint, and the same script, inputs,
fingerprint and budget give the same cost and the same failure point. Replaying
under another costing is not a faithful replay. Snapshots save the costing and
restore it. See `budget.md`.

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
- Verification covers the developer's own machines, which are x86_64: every
  sweep and golden runs on that one target, optimized and unoptimized.
  The aarch64 code (the hardware FMA instance, each kernel's aarch64 arm and
  the `FPCR` reader) follows the same contract but is not compiled, linted or
  run by the gate, and counts as unverified. musl, 32-bit targets with SSE2 and
  emulated runs are checked only if a player platform needs them. The library
  itself has no Unix dependency; only the repository's test suite and the
  `dellingr-bench` harness are Unix-only, so they do not build on Windows.
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
- Checked: every public fallible `State` method made by host code that can
  compute, except the value moves listed below, plus `Program::compile`,
  `save_state`, `load_state` on entry and again after its setup closure, and
  every host Rust callback's return, where a callback
  that returns `Ok` with the environment changed fails with this error
  instead, so Lua never resumes under it. A host function's own calls into
  the State are checked; a standard-library function's are not, since it
  only runs after a check that followed the last host code. A
  standard-library function's return is checked only for `print`, which
  calls `on_print` and can return right after it: every other library
  function runs no host code itself, and any host callback it reaches
  returns through its own check. The library functions the dispatch loop
  executes directly when Lua calls them (`math.floor`, `type`, `rawget`,
  ...) enter no callback and run no host code, so nothing is checked at
  their return. The process allocator is assumed to leave the environment
  as it found it, as it is during any allocating operation. A check covers
  only the work after it, so no caller code runs between a check and the
  work it guards: `push_string` and `push_bytes` take `&str` and `&[u8]`, so
  any conversion runs at the call site before the method is entered, and
  `Program::compile` checks after its `source.as_ref()`. Creating a State is not
  checked: it computes nothing that depends on the environment, and a State
  may be created on one thread and run on another.
- Refusing poison only, not the environment: the fallible methods whose
  outcome cannot depend on it, because they move values, validate integer
  indices and handles, or read tags and bytes, and run no Lua, host code,
  compilation, heap allocation or floating-point instruction on a value:
  `push_nil`, `push_boolean`, `push_number`, `push_value`, `push_anchor`,
  `push_rust_fn`, `pop`, `set_top`, `insert`, `remove`, `replace`,
  `copy_val`, `to_number`, `to_bytes`, `try_to_boolean` and `try_typ`
  (`scripts/poison_contract.py` holds the reviewed list). They succeed in an
  unsupported environment, and the refusal comes at the next method that
  computes or at the callback's return, before any Lua runs. What they did
  stays done: a value popped or pushed then is popped or pushed, as with any
  call that failed partway. Each read of the control register costs a few
  nanoseconds on x86_64, which made it most of the cost of a host call
  assembled from stack operations. `consume_cost` keeps the check, so no
  budget is spent in a refused environment.
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
- The host receives every NaN as that same canonical quiet NaN
  (`0x7ff8_0000_0000_0000`): `State::to_number`, `State::raw_field` and
  `State::raw_fields` read as `f64` or `Option<f64>`, a `State::invoke` result read the same way, and the
  `ErrorValue::Number` of `error(x)`. Inside
  the VM a NaN keeps whatever sign and payload the hardware or a host push
  gave it, but no exit lets those bits out. Every other number, `-0.0`
  included, reaches the host with its exact bits.

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

`tostring` and `%p` of tables, closures, `string.gmatch` iterators and Rust
functions render a state-local
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

Collections fire when the allocation count (host-set threshold) or the
pacing score of objects, strings and resident table and string bytes
(automatic mode, [the GC policy](limits.md#garbage-collection-policy)) reaches
the threshold at an allocation check. Automatic mode also checks at taken
Lua back edges, Lua call admission, and `table.move` copy boundaries.
Resident bytes follow table capacity growth, so their timing is
deterministic for a given build and anchor release history, subject to the
cross-thread drop requirement above. It is not a cross-version contract: a release
may move allocation points or collection thresholds, which can change `%p` ids
of rebuilt strings and `State::object_count()`. Such changes are called out in
the changelog, like other replay-visible changes.

## Error labels

Function labels in argument errors are diagnostic text, not stable
identifiers, and are not a cross-build log format.

- Standard-library validators that use their named helpers supply explicit
  names. These do not vary.
- Generic Rust callback checks use the active Rust function entry's diagnostic
  name, fixed when the entry was created. `State::add_rust_fn` and
  `State::register_rust_fn` take that name directly; the name passed to
  `State::set_global_named_rust_fn` is used for its entry. Standard-library
  entries use their catalog names.
- The name does not depend on globals, aliases or the `snapshot` feature. A
  host that registers its own function under a different name in another build
  can produce different diagnostic text.

Match on `ErrorKind`, not on `Display` text. See
`reference/error-reporting.md` for the naming rules.
