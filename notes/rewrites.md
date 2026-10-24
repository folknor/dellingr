# Rewrites

Large structural changes the verification hunt proposed. Each would remove a
group of smaller findings filed in the `verif-*.md` documents; those entries
stay filed on their own and name the rewrite here by title. Each entry names
the scopes whose hunters proposed it.

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

## RW-001 - Merge the ~80 integration-test binaries into a few grouped crates

Reported by: all seven scopes.

`tests/` holds about 78-80 top-level files, each its own integration crate
linking `dellingr`, in each `[[check]]` shape. Costs the hunters list:

- Linking every binary in every sweep is the dominant per-change cost of the
  test phase's prepare: 37.2s after a code change against 5.8s warm
  (check-machinery, from the check log). At a few hundred milliseconds per dev
  link, tens of seconds per sweep (runtime, unmeasured). Clippy
  `--all-targets` compiles them all too.
- A focused `brokkr test` lists and starts every harness per sweep.
- Seven files (`open_libs_registry.rs`, `save_state*.rs`, `save_golden.rs`,
  `ipairs_save_state.rs`) are `#![cfg(feature = "snapshot")]` instead of a
  `[[test]] required-features` entry, so in the default sweep cargo still
  compiles, links and spawns each as an empty harness. The last check journal
  shows them in lane 0 with `"suite_started","test_count":0`: a silent
  zero-test pass for targets whose names promise coverage. The example
  `regenerate_save_golden` already uses `required-features`, so the repo has
  both conventions.
- `tests/common/*` is pulled in per binary by `#[path]` with
  `#[allow(dead_code)]` on helpers (`cli.rs`, `stack.rs`,
  `scratch.rs::write_boundary_transcript`, and the whole module in
  `run_examples.rs`), so a helper that has become dead in every crate is never
  reported.
- Each library suite re-implements the same compile-load-call helper: about 30
  `fn run`/`run_bytes`/`expect_error`/`assert_runtime_error` definitions across
  `tests/`, and `gsub_errors.rs` inlines the sequence about 20 more times; the
  snapshot files duplicate `run`, `global_str`, `no_setup`, `reload`, `fresh`.
- Misplaced tests are invisible under focused runs by subject (see the
  misfiled-tests entry in verif-assertions.md).

Proposed shape (hunters differ on grouping): `autotests = false`, and either
one `tests/integration/main.rs` with a `mod` per current file, or a handful of
subject crates: `tests/runtime/`, `tests/cost/`, `tests/compiler/`,
`tests/library/` (with a shared `support` module and a capturing-callbacks
State), `tests/snapshot/` declared with `required-features = ["snapshot"]`.
Keep separate only what needs its own process: `panic_poison` (its panic
hook), `native_stack` (which the release sweeps select by name, and which can
abort the process), and the CLI and corpus suites that spawn processes.

Process-wide state that a merge would share, as inventoried by the hunters:

- `panic_poison.rs::quiet_deliberate_panics` installs a process-wide panic
  hook the first time any test calls it, permanently; it forwards everything
  but its exact message, but replaces whatever hook a later test sets and
  cannot be reset.
- `panic_poison.rs::AFTER_SWALLOW`, a `static AtomicUsize` read by one test;
  safe only because one test touches it. It belongs in the State's
  `user_data` like the other harnesses (`native_stack.rs`'s `Harness`,
  `host_error_reporting.rs`'s).
- `GMATCH_WRAPPER` (`src/lua_std/string.rs`), a process-global `OnceLock`;
  which test compiles it first decides envelope coverage (see
  verif-native-stack.md). Order-safe for correctness: `is_gmatch_wrapper`
  before initialization answers false.
- `NEXT_STATE_ID` (`src/vm/anchor.rs`) makes State creation order visible
  through `Anchor`'s `Debug`/`Hash`; no test observes it today.
- `scratch.rs`'s `NEXT_SCRATCH_ID` (safe: ids are unique per pid and counter);
  `hotpath.rs`'s FIFO `OnceLock` (harness only); crmath's `FORCED_FMA` and
  `KERNEL_BACKENDS` (thread-local).
- The crmath panic-hook swap (bugs.md "crmath/tests/golden.rs swaps the
  process-wide panic hook") is the one real hazard in the lib binary.

No test in any scope writes environment variables or installs a global logger.

Enforceable: `autotests = false` makes an undeclared file never compile, which
is its own hazard; pair it with a `test_conventions.rs` check that every
`tests/*.rs` and `tests/*/main.rs` is declared in `Cargo.toml`, and a rule that
a test file may not open with `#![cfg(feature = ...)]`. Once merged,
`dead_code` works without allows, and a rule "no `#[allow(dead_code)]` under
`tests/`" holds it. A textlint rule over `tests/**` banning
`set_var`/`remove_var`/`set_hook` and `static .*Atomic` outside one named
fixture module keeps the merged binary order-independent. A rule that a test
file using `save_state(`/`load_state(` lives under `tests/snapshot/` keeps
snapshot tests from re-scattering.

## RW-002 - Replace the shell differential gate with an in-process gate over checked-in expected output

Reported by: check-machinery, corpus-harness, library.

Today one `cargo test` process spawns seven `bash diff_test.sh` partitions
concurrently. Per file each runs `grep` three times, then `timeout dellingr`,
`timeout lua5.2` and `timeout lua5.4`, writes all three outputs to `mktemp`
files in `.scratch/`, and runs `diff -q`/`diff -u`. Per sweep, `run_examples`
executes all 82 examples under `bash test_limited.sh`, then
`differential_examples` executes them again: about 284 reference-Lua runs per
`brokkr check` across the two dev sweeps, producing the same bytes every time,
because reference output does not depend on dellingr's build.

The split creates, and this rewrite would remove:

- `shell_and_rust_example_corpora_match`, `malformed_timeout_is_rejected_once_before_other_checks`
  and `diff_markers_have_nonempty_reasons` (which re-implements a rule
  `diff_test.sh` already enforces, with Unicode `str::trim` against ASCII
  `[[:space:]]` under `LC_ALL=C`), all of which test the shell tool and run
  twice per check;
- the `.scratch/dellingr-diff-*` leak on SIGKILL;
- `bash`, GNU `timeout`, `grep`, `diff`, `find`, `sort`, `sed`, `mktemp` and
  `lua5.2`/`lua5.4` (Debian naming, distro patch versions, distro compat flags)
  as gate host requirements;
- the opaque failure shape (one test, seven partitions, a stderr blob);
- DIFF-marked files escaping comparison entirely, since they would carry
  dellingr's own reviewed expected output;
- the snapshot sweep re-running reference Lua whose output cannot depend on
  dellingr's features.

Two options, which compose:

1. Native gate: spawn dellingr and the references from Rust with
   `Stdio::piped()` and a deadline loop (already in `run_examples.rs`), compare
   bytes in memory, report per example. `diff_test.sh` becomes a manual tool
   or is deleted.
2. Checked-in expected output, e.g. `examples/**/<name>.expected` or
   `.lua.out`, generated by an explicit refresh script that runs both
   references, records `lua5.x -v` and which reference matched, and carries a
   hash of the source in a header, with a line-level way to record a
   deliberate divergence and its reason. The gate compares dellingr's output
   in Rust in the same pass that applies `run_examples`' rules, so the corpus
   runs once per sweep. A stale `.expected` (source-hash mismatch) is a check
   failure; reference drift becomes an on-demand audit.

Enforceable: yes. The refresh script's check mode and a test that every example
has expected output; the source-hash header makes staleness a test failure.

## RW-003 - One generated `#[test]` per example

Reported by: corpus-harness, check-machinery.

`run_all_examples` and `differential_examples` each run the whole corpus as
one libtest test, and both re-implement a worker pool because "brokkr runs test
binaries with one test thread". Costs: no focused run of one example (`brokkr
test calls_method` matches nothing); no per-example 20s cap or attribution, so
a hang anywhere is reported against the whole corpus; the in-test pools
hard-code their concurrency (see the timeout and concurrency entry in
verif-corpus.md).

Fix: generate one `#[test]` per example from a `build.rs` that walks
`examples/` and emits `include!`-able test functions (with
`cargo:rerun-if-changed=examples`). Each example then has a name, its own cap
and attribution, and can run alone; concurrency becomes brokkr's job
(`test_threads`, `parallel = { budget }`, or a `harness = "nextest"` sweep that
runs process-per-test). The same generated list can feed the
compiler-verification, disassembly, cost-golden and save-round-trip passes.

Enforceable: structural; the walker becomes the only enumeration.

## RW-004 - Identify Rust functions by registry handle instead of code address

Reported by: snapshot, runtime.

`State.rust_fn_ids_by_addr: BTreeMap<usize, String>` keys identity on `func as
usize`; every save of a `Val::RustFn` goes through it
(`saved_reachable_pointer_value`). `Val::RustFn` equality and table-key hashing
use `std::ptr::fn_addr_eq` (`lua_val.rs`), and `save_state.rs` and `vm.rs`
compare `as usize` addresses. Rust gives no guarantee that one function has one
address: identical functions can be merged, and inlined or generic functions
can be instantiated in several codegen units. Consequences:

- debug and release write different bytes (bugs.md "Save bytes differ between
  dev and release builds");
- load has a dedicated "folded functions" path (`materialize_payload`) and an
  error for folded keys colliding in one table, with two integrity tests
  (`folded_functions_*`) that simulate a build that cannot be produced on
  demand;
- `docs/save-state.md` needs two paragraphs of caveats;
- a host that registers an `#[inline] fn` from one crate and pushes it from
  another can hit `UnregisteredFunction` on a function it did register; no test
  can catch that, because every test registers and pushes from one crate;
- `f == g` for two host RustFns with identical bodies, and whether they collide
  as table keys, can differ between dev and release builds of the same host: a
  script-visible, build-dependent result.
  `lua_val.rs::different_rust_functions_compare_unequal` uses two functions
  with different bodies, so it cannot see this. The runtime hunter suggests at
  least a line in `docs/determinism.md` and a release-run test with two
  identical-bodied functions.

Fix: `Val::RustFn` carries a `RustFnId(u32)` into a per-State table of
`(stable id, fn pointer)` assigned at registration; an unregistered function
gets a handle with no stable id and saving it still fails fast. Identity
becomes registration; folding is irrelevant; the golden is
profile-independent; the fold-handling load path, its error variant and its
two integrity tests can go; `rawequal` between two registered ids of one
function becomes a design decision instead of a linker accident; tests get an
injection point (register two ids for one function). The snapshot hunter adds
that `open_libs` under `snapshot` builds a whole scratch `State::raw()` and
re-runs `open_libs_unchecked` whenever the registry is non-empty
(`check_registry_compatible`); with registry handles the compatibility check
could compare id lists without building a State.

Enforceable: by the type system; `func as usize` as identity stops existing.

## RW-005 - Hold runtime limits in a value tests can lower

Reported by: runtime, library, native-stack-errors (dissenting).

All runtime limits are crate constants (`limits::MAX_STACK_SIZE`,
`MAX_CALL_DEPTH`, `MAX_NATIVE_CALL_DEPTH`, `MAX_METAMETHOD_DEPTH`,
`TABLE_SORT_MAX_LEN`, `MAX_ARRAY_LEN`, `MAX_STRING_BYTES`). Every boundary
test must drive the real value: million-slot stack fills (see the at-cap
entry in verif-native-stack.md), 8-16 MiB strings in
`print_bounds_the_raw_joined_output_by_the_cap` and
`lossy_host_conversion_is_size_checked`, recursion to the real call depth, and
the snapshot backdoor that is the only way to reach `MAX_ARRAY_LEN` (see the
growth-limit entry in verif-assertions.md).

Proposal (runtime, library): a `Limits` value held by `State`, defaulting to
the constants, settable only through a `#[doc(hidden)]` or test-feature
constructor and refused by snapshot save. Contract tests use small limits; the
native-stack envelope and one test per limit keep the real values. Limits are
part of replay behaviour, so if a host could ever set them they would have to
be saved and checked on load; keeping the setter test-only avoids that. The
library hunter calls the choice a design decision, not a defect.

Dissent (native-stack-errors): the constants being `pub const` is right for a
replay contract; the answer is fixture shape (fill once, compute heights), not
a test-only override, because an overridable cap is a replay hazard and a
test-only path production can reach.

Enforceable: by type (production reads `self.limits`), plus a clippy
`disallowed` path or textlint on direct `limits::MAX_*` reads in `src/vm/`.

## RW-006 - Make `src/crmath/` its own workspace crate

Reported by: library.

The crmath, patterns and numeral tests have no feature cfg, yet the lib
unit-test binary is built and run under both the `default` and `snapshot`
shapes, so every check pays twice for `golden_results_on_every_fma_path`
(13,386 rows times 3 FMA modes), the `fma` sweeps (roughly 300k `check` calls,
each a libbeef infinite-precision multiply, add and round) and
`oracle_known_values`. There is no `[profile.dev.package.libbeef]` override, so
libbeef runs at opt-level 0. Unmeasured; the largest pure-CPU unit-test block
in that scope. And because crmath is a module of the one crate, any edit
anywhere in the VM recompiles about 570 KB of generated tables (about 5,000
lines of const-evaluated `hf("...")`) in every build shape, including both
clippy passes, and relinks the lib test binary that carries the oracle.

Fix: a `dellingr-crmath` workspace crate with its own tests, tables and libbeef
dev-dependency. Its compile is cached across VM edits, its tests run once, it
gets a clean target for a release check (see the cross-build entry in
verif-goldens.md), and "crmath depends on nothing in the VM" becomes a
dependency rule. Independently: `[profile.dev.package.libbeef] opt-level = 3`,
a one-line measurable win; optionally have the generator emit
`f64::from_bits(0x...)` with the hex spelling in a comment, checking exact
representability once at generation time.

Enforceable: the crate boundary is a structural rule; the profile override is
config.

## RW-007 - Make State's GC-root and in-flight inventories exhaustive by construction

Reported by: runtime, snapshot.

AGENTS.md: "`vm::mark_gc_roots` is the *single source of truth* for what's
reachable." `mark_gc_roots` reads State fields one by one; nothing makes a new
State field holding `Val`s show up there. The same holds for the `in_flight!`
macro ("New fields must be classified here"), which feeds
`debug_assert_at_rest`, `validate_quiescent` and `reset_in_flight`. A field
added and forgotten is caught only if some test collects or saves at the right
moment. The tests that look like guards claim more than they check:

- `save_walker_object_count_matches_gc_live_minus_env` says "Adding a GC root
  without teaching the save walker would break this identity". That holds only
  if the test's program puts an object into the new root. Anchors are the
  counterexample already in the tree: a GC root the walker deliberately does
  not follow, and the identity holds only because the program creates no
  anchor.
- `reset_and_quiescence_share_the_snapshot_inventory` dirties `stack_bottom`,
  `pending_bytecode_caches`, the transient roots and the three depths, but not
  `open_upvalues`, `call_stack`, `vararg_stack` or `active_rust_fn`, and
  nothing forces a new in-flight field into the macro list at all (pooled
  `frame_stacks` is a candidate worth a look).

`src/vm/caches.rs::drop_dead_entries` shows the repository already knows the
fix: it destructures every struct without `..` "so a new cache family, cell or
handle field fails to compile until its verdict is decided".

Fix: write `mark_gc_roots` as an exhaustive `let State { stack, global_env,
..., rng: _, user_data: _ } = state;`, each ignored field with a one-line
reason, and give it a root-category enum the save walker must `match`
exhaustively ("walked", "saved by token", "deliberately not saved: reason").
Move the in-flight fields into one `InFlight` sub-struct with
`#[derive(Default, PartialEq)]`; quiescence is `self.in_flight ==
InFlight::default()` and reset is `= InFlight::default()`, and the test reduces
to "default is quiescent".

Enforceable: yes, structurally (exhaustive destructuring and match, a type).
