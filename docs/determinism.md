# Determinism

The same script and inputs produce the same result and the same cost on a
given build. The library reads no host state: no environment, clock, process
arguments, unseeded RNG or randomized hashing. The CLI and the bench harness
are hosts and are exempt.

## Math

- `math.random` draws from the in-crate `VmRng` (SplitMix64), seeded by
  `set_rng_seed` (default seed 0). A fixed seed gives the same stream on every
  platform.
- Basic arithmetic, comparisons, `sqrt` and `math.random` are bit-stable
  across platforms.
- `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log` and the
  `^` operator use the platform `f64` / libm. Results are deterministic per
  build and platform, but the last ULP can differ across architectures or
  libm versions.
- The compiler never constant-folds `^`, so bytecode and snapshots carry no
  host-computed `^` results. A replay evaluates every `^` on the replaying
  host.
- The authoritative contract is the module doc of `src/lua_std/math.rs`.
- Replays that must be bit-identical across heterogeneous hosts should not
  depend on exact results from the libm-backed functions.

## Number rendering

- `tostring` and `string.format` render numbers with `%.14g`, independent of
  platform: `0.3`, `1e+15`, `-0`.
- NaN renders unsigned (`nan`) everywhere. No script can observe the sign or
  payload of a NaN.
- Snapshots write every NaN as one canonical NaN. See `save-state.md`.

## `pairs` order

- A table's array part (keys `1..n`, grown by appending `n + 1`) is visited
  first, in key order.
- Every other key follows in insertion order.
- Snapshots preserve the array part's shape, so a loaded state continues with
  the same order and `#` results.

## Object identity in output

`tostring` and `%p` of tables, closures and Rust functions render a state-local
identity, not an address. It is stable across a snapshot round trip. Rust
functions render as `function: 0x<id>`.

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
