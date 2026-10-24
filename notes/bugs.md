# Bugs

Defects found during the verification hunt over the whole tree (scopes:
check-machinery, corpus-harness, native-stack-errors, snapshot, compiler-cost,
runtime, library). Each entry names the scopes whose hunters reported it.

```
1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.
```

---

## BUG-002 - `hotpath-alloc` does not enable this crate's `hotpath` feature, so `cfg(feature = "hotpath")` guards fail open

Reported by: corpus-harness, native-stack-errors, compiler-cost.

`Cargo.toml` defines `hotpath-alloc = ["hotpath/hotpath",
"hotpath/hotpath-alloc"]`. That enables the dependency's features, not this
crate's own `hotpath` feature. Under `--features hotpath-alloc` the measure
guards are live (they key on the dependency's feature) while
`cfg(feature = "hotpath")` is false. Guards spelled with `"hotpath"` alone
therefore fail open under `hotpath-alloc`:

- `tests/integration/corpus/diff_test.rs` compiles its corpus partitions out only for
  `"hotpath"`, for the stated reason that instrumentation pushes the corpus
  past the time budget. Under `hotpath-alloc` they compile in and run
  instrumented, contrary to AGENTS.md ("The corpus partitions are compiled out
  under the `hotpath` feature").
- `src/compiler/parser.rs` sets `DEEP_PARSE_TEST_STACK_BYTES` to 8 MiB under
  `cfg(feature = "hotpath")` only, so instrumented `hotpath-alloc` parser unit
  tests parse at the syntax budget on 2 MiB with timing guards on every parser
  frame, which is what the 8 MiB branch exists to prevent.
  `tests/native_stack.rs` sets `PARSE_TEST_STACK_BYTES` (and
  `tests/common/stack.rs` sets `LIMIT_TEST_STACK_BYTES`) to 8 MiB under
  `hotpath` *or* `hotpath-alloc`, and says its constant matches "as
  `src/compiler/parser.rs`'s unit-test constant does"; that parity claim is
  false for `hotpath-alloc`.

`tests/common/stack.rs`, `tests/native_stack.rs` and
`tests/integration/failures/error_boundary.rs` spell both features and are
correct, so the repository already knows; the guards disagree.

Fix: `hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]`, so
`cfg(feature = "hotpath")` means "instrumented" everywhere and the
`any(hotpath, hotpath-alloc)` spellings collapse. Then keep the parse-test
stack size in one place both the unit tests and the integration tests read (a
`#[doc(hidden)] pub` constant in `limits`, a `cfg(test)` export, or a
`tests/common/stack.rs` that `src/` includes the way it already includes
`tests/common/examples.rs`).

Enforceable: the feature implication is structural (Cargo enforces it). A
textlint rule banning `feature = "hotpath-alloc"` inside `cfg(` anywhere except
the allocator installs (`src/main.rs`, `examples/hotpath.rs`) keeps it so. One
constant instead of two removes the drift.

## BUG-007 - Save bytes differ between dev and release builds of one version, while the docs and the golden say otherwise

Reported by: corpus-harness, snapshot, compiler-cost.

`tests/integration/snapshot/save_golden.rs` documents that the fixture "must come from the profile
the tests run under": a release build (`lto = "fat"`) can fold
`base_unpack_values` and `table_unpack_values` (two byte-identical wrappers
around `unpack_values` in `src/lua_std/basic.rs` and `src/lua_std/table.rs`),
and the save then writes the lexicographically smallest id (`table.unpack`)
for the global `unpack`. The golden program loads the standard library, so
every builtin global is in the save as an `Fn` token and folding reaches the
bytes. Consequences:

- `docs/save-state.md` "Byte stability" says "Two saves of the same logical
  state are byte-identical with the same crate version", and the module doc of
  `src/vm/save_state.rs` repeats the crate-version wording. The same docs
  file's "Rust functions" section says "snapshot bytes are only guaranteed
  identical across the same build". They contradict each other, and the
  crate-version wording is false. The module doc's "because the diagnostic
  crate-version string is part of the blob" also inverts the logic: the string
  makes saves differ across versions, it does not make them identical within
  one.
- The pinned bytes are a debug build's; the bytes a shipped release game
  writes are pinned by nothing.
- Nothing in the test detects its profile. Run under release, it fails with
  "the walker reordered the object graph and every existing save file is now
  unreadable", which is the wrong diagnosis (a pure traversal reorder does not
  make old saves unreadable either; ids are dense indices). `brokkr test
  save_golden --release` fails on correct code. (A plain `brokkr test
  save_golden` no longer reaches a release sweep: brokkr now keeps each
  sweep's `tests = ["native_stack"]` selection.)
- `examples/regenerate_save_golden.rs` does not check its profile; run with
  `--release`, it silently writes a fixture every dev sweep then rejects.
- The failure message offers only "format or traversal change"; a
  `COST_MODEL_VERSION` bump also changes bytes and fails it.

Fix, in order of preference given by the hunters: remove address identity
from Rust functions (the registry-handle rewrite in rewrites.md), after which
the golden is profile-independent and can run in a release sweep; failing
that, build the golden State with `State::raw()` plus only the functions the
program needs so no foldable pair is reachable, or normalise function ids
before comparing; at minimum mark the test
`#[cfg_attr(not(debug_assertions), ignore = "...")]` and have the regenerator
refuse to run unless `cfg!(debug_assertions)`. Correct the docs and the
failure message either way.

Enforceable: once profile-independent, add `save_golden` to a release
`[[check]]`. The registry-handle rewrite makes the hazard unrepresentable.
