# Calls back into Lua: the native-stack guard

Every call made from Rust into a function runs on the native stack and
counts against `MAX_NATIVE_CALL_DEPTH`. What one such level costs is the sum
of the fixed frames on the compiled path that made the call: the dispatch
loop, the opcode handler, the metamethod or library code between them, and
`call_unguarded`. docs/limits.md, "Envelope", promises that every limit at
once fits a stated thread size, and `tests/native_stack.rs` backs that
promise by running each such path to the limit. A path the test does not
know about is a path the promise does not cover, so the set of paths is
kept current mechanically:

- `reference/native-calls.toml` is the inventory: every function that can
  reach a call back into Lua, every call site between them (an edge), and
  every scenario of the test with the ordered edges of its path.
- Every call to an inventoried function in `src/` carries a
  `// native-call: <edge-id>` comment naming its edge.
- `scripts/native_calls.py` checks the inventory against the source and the
  test and prints `ok` when they agree; `brokkr check` runs it as the
  `native-call-inventory` script check. Fixture tests for the lexical model
  run through `scripts/test_native_calls.py` as a separate script check.
- The `native-call-marker` textlint rule in `brokkr.toml` flags any call to
  an inventoried name without a marker, so an editor or a plain
  `brokkr check` points at the line before the script's report is read.
- The `native-call-method-value` textlint rule flags qualified method values
  on `State`, `Self` and `Frame` using the same inventoried names. The script
  checks that its alternation stays current too, and that it scans the same
  files and regions as the call rule. This is a tripwire for
  turning a direct method call into an invisible function-pointer call.

Markers are comments. Adding one never changes code: frames that stay live
across a native re-entry are size-sensitive (AGENTS.md, "Hotpath
benchmarks"), and `brokkr test native_bytes_per_level` in the dev and
release profiles must report the same numbers before and after.

## The inventory

The functions are the caller closure of the two execution boundaries:
`State::call_unguarded`, which every call made from Rust goes through, and
`State::invoke_rust_fn`, which runs every Rust callback. The closure stops
only at roots, functions that are also entered from outside the lexical call
graph, each listed with its kind and the evidence for it:

- `callback`: a library callback, entered through a function pointer by
  `invoke_rust_fn`. A callback written as a closure in a registration list
  (`("name", |state| { ... })` or `add!("name", |state| { ... })`) is a
  function of its own named `lib:name`, prefixed with its library
  (`lib:table.sort`; the base library has no prefix, `lib:print`).
- `host-api`: a `pub fn` on `State` that a host, or a host callback, calls.
- `binary`: the CLI's `main`.

An edge id is `caller->callee`, with a `:tag` when one caller calls one
callee from several sites (`run_frame->reg_get_field:field` and `...:self`).
Each edge is either covered or excluded:

- covered by the scenarios whose path runs it, or by the `chains` or
  `leaves` it names (the metamethod chain kinds and deepest-point work in
  `tests/native_stack.rs`, for edges that only those reach);
- or `excluded`, with the reason the call can never reach Lua from that site
  (the default sort comparison, a raw `table.move` copy, the `%%` directive)
  or why it lies outside the envelope (the CLI's own call).

A scenario's path is one recursion cycle: it starts at an edge out of
`call_unguarded` and ends with an edge into it. Consecutive edges must
connect, except that an edge into `invoke_rust_fn` may be followed by an
edge out of a `callback` or `host-api` root, the function pointer the
callback was entered through. Named `[segment]` runs of edges keep shared
prefixes in one place; `@name` in a path expands one. A cycle can take
several native levels: its number of edges into `call_unguarded` must
equal the scenario's `native` field in `tests/native_stack.rs`.

## What the script checks

- The inventoried functions are exactly the computed caller closure, and a
  function without callers in `src/` is a classified root whose evidence
  holds: a callback has the `RustFunc` signature and is used as a value, a
  host-api root is a `pub fn` on `State`, a binary root is `src/main.rs`'s
  `main`.
- No function in that closure, either execution boundary, `run_invocation`
  or `Frame::run` carries `hotpath::measure`. Its guard would remain live
  across native re-entry and include the nested script's work in its time.
- Every call to an inventoried function has exactly one marker, on its line
  or the line directly above, and the marker's caller and callee are the
  enclosing function and the called one. Every marker belongs to a call,
  every edge id marks exactly one call, and every call site of one pair is
  tagged when there are several.
- Every edge is covered or excluded, never both; every chain and leaf an
  edge names exists; every scenario path connects and its native count agrees with the test.
- The scenarios in the inventory are exactly the `SCENARIOS` list of
  `tests/native_stack.rs`, and the chain and leaf names exist in its
  `CHAINS` and `LEAVES` lists.
  Names are read only from direct entry fields in active source text;
  commented-out entries and nested fields do not supply coverage. This is
  a lexical check, not proof that the test or every entry executes.
- The textlint rule's pattern and `except` equal the inventory's names, and
  nothing outside the rule's `exclude` matches the pattern without being a
  call to an inventoried function. `src/compiler/` is excluded because it
  holds no State and its `Instr::call`, `get_table` and `set_table`
  constructors share names with inventoried functions; a call into an
  inventoried function from an excluded file is reported.

## The lexical model and the direct-call convention

The script reads source text, not a compiler call graph. It finds `fn`
items and their bodies, skips `#[cfg(test)]` items and out-of-line module
subtrees declared under `cfg(test)` or `cfg(all(test, ...))`, and
attributes a closure's body to the function that contains it, so a call
inside `self.guarded(|state| ...)` or `with_metamethod_depth(|state| ...)`
belongs to the enclosing function. Higher-order helpers that only invoke
such a closure (`guarded`, `with_metamethod_depth`, `with_rooted_values`)
are frames on the path but not inventoried: the scenarios run them.
Test-like filenames alone never exclude a source file from scanning.

A call resolves by name and receiver. That is the convention re-entering
functions are called by, and new code should keep to it:

- `self.f(...)` inside `impl T`, and `Self::f(...)`, resolve to `T::f`;
- `state.f(...)` resolves to `State::f`, so a `&mut State` parameter is
  named `state`;
- `T::f(...)` resolves to `T::f`;
- a bare or module-path `f(...)` resolves to a free function;
- any other receiver resolves to every method named `f`, which is safe but
  may ask for markers on calls that cannot re-enter.

Method chains split over lines keep the name and its `(` on one line, as
rustfmt writes them, so the marker goes on the line of the name or directly
above it.

Calls through function pointers, closures and trait objects are invisible to
a lexical model, so each kind has a fixed answer. Rust callbacks are entered
only through `invoke_rust_fn`, which is a boundary. A closure handed to a
helper is attributed to the function that wrote it. A host closure
(`with_restricted_env`, the snapshot loader's setup) is host code, which
reaches Lua through the `host-api` roots. The table sort's
`TableSortAccess` implementations read and write raw. A new indirect call
that can reach Lua needs this guard extended before it lands.

A blanket ban on inventoried names used as values would reject the callback
registrations the inventory explicitly requires: the base ipairs iterator,
the base and table unpack callbacks, and string format. The method-value
tripwire therefore leaves free functions and other qualified owners to
review. Aliased owners, wrapped paths and new trait-object entry paths also
need review; this lexical rule is not a compiler call graph.

## Adding a path

When a change adds a call that can reach Lua, `brokkr check` reports the
unmarked call (textlint and script) and, for a new re-entering function,
that it is not listed. Then:

1. Add the function to `[[function]]` (with its `root` kind and evidence if
   it is entered from outside), the call site to `[[edge]]`, and the marker
   to the call.
2. If the edge opens a new path, add a `Scenario` to `tests/native_stack.rs`
   that recurses through it (its `native` and `meta` per cycle, a `DESCEND`
   inside the handler the path calls, and Lua checks of any returned value),
   its name in the `scenario_tests!` list there (one test per scenario), and
   a `[[scenario]]` with the same name and its path. If only a chain
   kind or a deepest-point item reaches it, name those on the edge instead.
3. If the call can never reach Lua, or is outside the envelope, mark the
   edge `excluded` with the reason.
4. Run `brokkr test native_bytes_per_level` with `--debug` and `--release`
   to see the new path's bytes per level, and `brokkr check`, which runs the
   envelope in both profiles.

A path that leaves a dispatch arm (`run_frame->...`) must also record the
instruction as the activation's call site before anything on it can call,
or tracebacks and `print` locations name a stale line. The arms do not
record on every execution: `State::record_call_site` explains where each
existing path records, which for the table and length instructions is the
first line of their outlined handler-call function (`index_call`,
`newindex_call`, `len_call`). The resolvers before those calls
(`State::resolve_index`, `store_field`, `resolve_newindex`,
`resolve_length`) call nothing and have returned before the handler runs,
so they are not inventoried and add no frame to a path.

`python3 scripts/native_calls.py sites` lists every call site with its
marker, which is the quickest way to see what a change touched.

## Measured native stack, 2026-10-23

Measured on x86-64 Linux with the repository's dev and release settings.
The baseline is 97973d1; the middle column of each cell is unmodified
22a263a; the last is 22a263a with `run_invocation` forced inline into both
`call_unguarded` monomorphizations. These are compiler-dependent measurements,
not additional limits or assertions of headroom for another build.

`brokkr test native_bytes_per_level` runs all four configured sweeps;
`--release` runs the two release feature shapes. The default and snapshot dev
figures agree in each revision. An old checkout's probe needs an isolated
`CARGO_TARGET_DIR`: sharing the current checkout's target directory reused
current binaries in the first baseline attempt. The isolated rerun rebuilt
all four shapes. Both release harnesses were built with one
`brokkr dellingr --lua queue` run (with `--commit 97973d1` for the baseline).

Each cell below is **97973d1 / 22a263a / inlined**, in bytes per scenario
cycle. `index_rust` and `newindex_rust` take two native levels per cycle;
other scenarios take one. The two `chain_` rows instead measure a single
metamethod table step, without a native level. Transient deepest-point work
is excluded; the full-depth envelope tests cover it separately.

| Scenario | Dev, also snapshot | Release | Release + snapshot |
|---|---|---|---|
| `index_field` | 10368 / 10576 / 10544 | 1680 / 1776 / 1648 | 1728 / 1840 / 1712 |
| `index_method` | 10368 / 10576 / 10544 | 1680 / 1776 / 1648 | 1728 / 1840 / 1712 |
| `index_key` | 9104 / 9360 / 9328 | 1360 / 1456 / 1328 | 1408 / 1520 / 1392 |
| `index_rust` | 11744 / 12032 / 12304 | 2064 / 2192 / 2112 | 2112 / 2256 / 2176 |
| `newindex_field` | 10096 / 10272 / 10240 | 1424 / 1520 / 1392 | 1472 / 1584 / 1456 |
| `newindex_key` | 9232 / 9472 / 9440 | 1392 / 1488 / 1360 | 1440 / 1552 / 1424 |
| `newindex_rust` | 11472 / 11728 / 12000 | 1808 / 1936 / 1856 | 1856 / 2000 / 1920 |
| `length` | 7840 / 8016 / 7984 | 1200 / 1296 / 1168 | 1248 / 1360 / 1232 |
| `string_index_field` | 10880 / 11072 / 11040 | 1728 / 1824 / 1696 | 1776 / 1888 / 1760 |
| `string_index_key` | 9616 / 9856 / 9824 | 1408 / 1504 / 1376 | 1456 / 1568 / 1440 |
| `index_string_handler` | 12112 / 12336 / 12304 | 1920 / 2016 / 1888 | 1968 / 2080 / 1952 |
| `string_library_index` | 11696 / 11904 / 11872 | 1920 / 2016 / 1888 | 1968 / 2080 / 1952 |
| `host_get_table` | 9744 / 10032 / 10000 | 1632 / 1728 / 1600 | 1680 / 1792 / 1664 |
| `host_get_table_string` | 10288 / 10560 / 10528 | 1680 / 1776 / 1648 | 1728 / 1840 / 1712 |
| `host_call` | 8384 / 8592 / 8560 | 1328 / 1424 / 1296 | 1376 / 1488 / 1360 |
| `host_anchor` | 8880 / 9088 / 9056 | 1440 / 1536 / 1408 | 1488 / 1600 / 1472 |
| `host_tostring_bytes` | 9216 / 9424 / 9392 | 1696 / 1792 / 1664 | 1744 / 1856 / 1728 |
| `host_to_string_with_meta` | 9376 / 9584 / 9552 | 1728 / 1824 / 1696 | 1776 / 1888 / 1760 |
| `tostring` | 8592 / 8800 / 8768 | 1504 / 1600 / 1472 | 1552 / 1664 / 1536 |
| `print` | 8976 / 9184 / 9152 | 1664 / 1760 / 1632 | 1712 / 1824 / 1696 |
| `format` | 11056 / 11264 / 11232 | 2000 / 2096 / 1968 | 2048 / 2160 / 2032 |
| `error_tostring` | 9136 / 9344 / 9312 | 1648 / 1744 / 1616 | 1696 / 1808 / 1680 |
| `sort_comparator` | 10208 / 10656 / 10624 | 1904 / 2032 / 1904 | 1952 / 2096 / 1968 |
| `sort_comparator_right` | 10208 / 10656 / 10624 | 1904 / 2032 / 1904 | 1952 / 2096 / 1968 |
| `sort_comparator_sortdown` | 10208 / 10656 / 10624 | 1904 / 2032 / 1904 | 1952 / 2096 / 1968 |
| `callable_rust` | 10624 / 11072 / 11040 | 2144 / 2272 / 2144 | 2192 / 2336 / 2208 |
| `sort_metamethods_comparator` | 9696 / 9904 / 9872 | 1840 / 1968 / 1840 | 1888 / 2032 / 1904 |
| `sort_metamethods_length` | 8800 / 9008 / 8976 | 1584 / 1712 / 1584 | 1632 / 1776 / 1648 |
| `sort_metamethods_read` | 10048 / 10336 / 10304 | 1760 / 1888 / 1760 | 1808 / 1952 / 1824 |
| `sort_metamethods_write` | 9936 / 10144 / 10112 | 1728 / 1856 / 1728 | 1776 / 1920 / 1792 |
| `insert_length` | 8752 / 8976 / 8944 | 1536 / 1632 / 1504 | 1584 / 1696 / 1568 |
| `insert_shift_read` | 10000 / 10304 / 10272 | 1712 / 1808 / 1680 | 1760 / 1872 / 1744 |
| `insert_shift_write` | 9904 / 10128 / 10096 | 1600 / 1696 / 1568 | 1648 / 1760 / 1632 |
| `insert_value` | 9904 / 10128 / 10096 | 1680 / 1776 / 1648 | 1728 / 1840 / 1712 |
| `remove_length` | 8736 / 8944 / 8912 | 1536 / 1584 / 1456 | 1584 / 1648 / 1520 |
| `remove_read` | 9984 / 10272 / 10240 | 1712 / 1760 / 1632 | 1760 / 1824 / 1696 |
| `remove_shift_read` | 9984 / 10272 / 10240 | 1712 / 1760 / 1632 | 1760 / 1824 / 1696 |
| `remove_shift_write` | 9888 / 10096 / 10064 | 1600 / 1728 / 1600 | 1648 / 1792 / 1664 |
| `remove_clear` | 9888 / 10096 / 10064 | 1680 / 1728 / 1600 | 1728 / 1792 / 1664 |
| `concat_length` | 10016 / 10224 / 10192 | 1584 / 1680 / 1552 | 1632 / 1744 / 1616 |
| `concat_read` | 11264 / 11552 / 11520 | 1760 / 1856 / 1728 | 1808 / 1920 / 1792 |
| `move_read` | 10320 / 10608 / 10576 | 1824 / 1920 / 1792 | 1872 / 1984 / 1856 |
| `move_write` | 10224 / 10432 / 10400 | 1712 / 1808 / 1680 | 1760 / 1872 / 1744 |
| `unpack_length` | 8608 / 8816 / 8784 | 1536 / 1632 / 1504 | 1584 / 1696 / 1568 |
| `unpack_read` | 9856 / 10144 / 10112 | 1712 / 1808 / 1680 | 1760 / 1872 / 1744 |
| `gsub_empty_function` | 10992 / 11200 / 11168 | 2000 / 2080 / 1952 | 2048 / 2144 / 2016 |
| `gsub_plain_function` | 11024 / 11232 / 11200 | 2048 / 2128 / 2000 | 2096 / 2192 / 2064 |
| `gsub_pattern_function` | 14432 / 14640 / 14608 | 3632 / 3712 / 3584 | 3680 / 3776 / 3648 |
| `gsub_pattern_captures` | 14432 / 14640 / 14608 | 3632 / 3712 / 3584 | 3680 / 3776 / 3648 |
| `gsub_empty_table` | 11904 / 12192 / 12160 | 2192 / 2272 / 2144 | 2240 / 2336 / 2208 |
| `gsub_plain_table` | 11936 / 12224 / 12192 | 2240 / 2320 / 2192 | 2288 / 2384 / 2256 |
| `gsub_pattern_table` | 15344 / 15632 / 15600 | 3824 / 3904 / 3776 | 3872 / 3968 / 3840 |
| `tfor_lua` | 7632 / 7808 / 7776 | 992 / 1088 / 960 | 1040 / 1152 / 1024 |
| `tfor_ipairs` | 10144 / 10416 / 10384 | 1536 / 1632 / 1504 | 1584 / 1696 / 1568 |
| `tfor_host_iterator` | 8976 / 9168 / 9136 | 1200 / 1376 / 1248 | 1248 / 1440 / 1312 |
| `chain_index` | 1200 / 1232 / 1232 | 192 / 192 / 192 | 192 / 192 / 192 |
| `chain_newindex` | 1216 / 1216 / 1216 | 208 / 208 / 208 | 208 / 208 / 208 |

### Frame attribution

Release harness prologues measured with `scripts/frame_sizes.py` (stack
allocation plus pushes, excluding the return address). `inline` means that
there is no separate frame on this path. Sizes are **97973d1 / 22a263a /
inlined**, in bytes.

| Function | Frame sizes |
|---|---|
| `call_unguarded`, ordinary | 168 / 120 / 168 |
| `call_unguarded`, entry observer | absent / 120 / 168 |
| `begin_call` | inline / inline / inline |
| `begin_callable_call` | 232 / 232 / 232 |
| `run_invocation` | inline / 168 / inline |
| `Frame::run` | 808 / 776 / 776 |
| `invoke_rust_fn` | 120 / 120 / 120 |
| `call_from_lua` | 120 / 120 / 120 |
| `get_table_unguarded` | 120 / 120 / 120 |
| `get_table_with_key_tracking_call` | 184 / 184 / 184 |
| `get_field_slow` | 488 / 488 / 488 |
| `get_table_slow` | 168 / 168 / 168 |
| `set_field_slow` | 216 / 216 / 216 |
| `set_table_slow` | 184 / 184 / 184 |
| `set_table_with_key` | 200 / 200 / 200 |
| `len_slow` | 200 / 200 / 200 |
| `index_string` | 232 / 232 / 232 |
| `base_ipairs_iter` | 88 / 88 / 88 |
| `tostring_bytes_unguarded` | 56 / 56 / 56 |
| `tostring_rendering` | 168 / 168 / 168 |
| table sort callback | 184 / 216 / 216 |
| table remove callback | 136 / 88 / 88 |
| gsub callback | 168 / 152 / 152 |

The index/newindex handler helpers and generic-for Lua iterator helpers have
no separate release harness symbols: they are inlined into the listed
parents, including `Frame::run`. `--calls` shows that the baseline's
`call_unguarded` calls `Frame::run` directly, whereas unmodified 22a263a calls
a separate `run_invocation`. Restoring inlining removes 128 bytes including
the extra return address from each ordinary release cycle. A Rust-handler
cycle saves 80 bytes: its Rust-only call arm has no live invocation driver,
and its now-combined call frame grows by 48 bytes instead.

The sort callback's 32-byte growth is retained. Its `--calls` and `--body`
diffs show the new fallible `get_array`, root-copy reservations and
`set_array` paths replacing infallible allocation and cloning. `asm_diff.py
--slots` shows the changed temporary and spill layout (14 distinct offsets
before, 21 after). Removing those checks would undo ordinary-error handling
for table allocation failures. The smaller dispatch frame cancels that
growth in the default release comparator cycle; snapshot's compiler layout
leaves that cycle 16 bytes larger than the baseline (until the vararg arena
follow-up below, whose 16-byte snapshot saving brings it back level).

Dev probe binaries also explain the retained growth: `Frame::run` is
6456 / 6568 bytes (baseline / inlined), `invoke_rust_fn` 264 / 280,
`call_from_lua` 200 / 216, the index handler body 600 / 632,
`instr_set_table` 344 / 376, `reg_set_table` 328 / 360,
`instr_get_field` 1640 / 1592, and raw `table_sort` 664 / 904.
These reflect fallible table operations, entry observation and the retained
result-copy path. The baseline's call and invocation frames are 344 and 328
bytes; the inlined ordinary call is 712 and the observed call 760. Folding
the driver into the Rust-only arm therefore increases the two-native-level
Rust-handler cycles in dev, although ordinary cycles shrink by 32 bytes
relative to unmodified 22a263a. Both dev feature shapes still pass the
unchanged 4 MiB full-depth envelope.

### Shared vararg arena follow-up

The shared vararg arena preserves every number in the **inlined** column
above for dev (both feature shapes) and plain release. Release with snapshot
saves 16 bytes in every native scenario cycle, including the two-native-level
Rust-handler cycles; `chain_index` and `chain_newindex` remain 192 and 208.
All 57 scenarios were rerun in all four shapes with
`brokkr test native_bytes_per_level`, followed by the explicit `--release`
run (default and snapshot). Representative final bytes per cycle:

| Scenario | Dev, also snapshot | Release | Release + snapshot |
|---|---|---|---|
| `index_field` | 10544 | 1648 | 1696 |
| `index_rust` | 12304 | 2112 | 2160 |
| `host_call` | 8560 | 1296 | 1344 |
| `tfor_lua` | 7776 | 960 | 1008 |
| `tfor_ipairs` | 10384 | 1504 | 1552 |
| `tfor_host_iterator` | 9136 | 1248 | 1296 |
| `gsub_pattern_function` | 14608 | 3584 | 3632 |

`frame_sizes.py` on the rebuilt plain release harness confirms every frame
listed in the attribution table is unchanged from **inlined**: both
`call_unguarded` copies are 168, `begin_callable_call` 232, `Frame::run` 776,
`invoke_rust_fn` and `call_from_lua` 120; `begin_call` and `run_invocation`
remain inline. The table sort/remove/gsub callback frames remain 216/88/152.
The only other changed VM symbol is `return_to_caller`, 72 -> 56; it does not
stay live across re-entry. The release snapshot probe has `Frame::run` 824,
both call copies 168, callable resolution 232 and Rust invocation 120. Dev
still has dispatch 6568, ordinary/observed calls 712/760 and Rust invocation
280. No frame live across native re-entry grows.

The arena uses a u32 start offset per activation, including empty
suffixes, keeping the x86-64 activation record at 24 bytes. Call depth times
MAX_ARGUMENTS bounds the arena below u32::MAX, checked at compile time.
Admission appends extra arguments and pop_activation truncates to the recorded start on return,
refused admission and unwind. The collector marks only the arena's live
length; quiescence still requires it empty. Capacity remains unsaved, and
the snapshot format and opcode charges are unchanged. Warm vararg-heavy
calls should avoid one allocation and free per call after arena capacity
has reached the active high-water mark.

Iterator bookkeeping was tried both inside the iterator helper and behind
a separate direct-path probe in the dispatch arm. Plain release dispatch
grew from 776 to 792 and 808 bytes respectively. Removing the Rust iterator
fallback's redundant error truncate did not reduce either figure. The eager
record/flush stays, with the reason at its site pointing here; pairs/ipairs are
therefore expected to stay unchanged.

Rust callee removal also stays. The contiguous callback ABI exposes every
slot freed by removing the callee; retaining it below the arguments would
refuse a push one slot earlier at the global cap. A regression fills the
stack before calling a one-argument Rust function that then pushes its
result. Moving the function elsewhere still relocates the arguments, while
a separate result destination alone cannot recover that slot during the
callback. No rust_boundary or ordinary calls speedup is expected from this
item. Nested arena GC/reuse and callback stack-cap regressions supplement
the existing refusal, error-unwind and snapshot-quiescence coverage. A nested
vararg case was also added to the budget and single-refusal corpus so failed
admission and open-vararg growth check arena quiescence explicitly.

### `__index` walk follow-up

Measured on plantasjen (x86-64 Linux), 4a722cf against the working tree on
top of it in which `__index` table steps became a loop
(`State::walk_index_tables`): the receiver's raw lookup and every handler
table are read in one outlined function, which also takes the handler's
depth unit and pushes its call window, so the frames that stay live across
the handler call are only the caller's. A table chain no longer recurses
natively, `call_string_index_handler` and the separate handler dispatch
frame are gone, and `index_string` outlines its metatable prelude
(`State::string_index_handler`). Cells are **4a722cf / after**, bytes per
cycle; scenarios not listed are unchanged, except that every `__newindex`
dev cycle is 48 bytes smaller (layout only).

| Scenario | Dev, also snapshot | Release | Release + snapshot |
|---|---|---|---|
| `index_field`, `index_method` | 10544 / 9296 | 1648 / 1520 | 1696 / 1568 |
| `index_key` | 9328 / 8432 | 1328 / 1264 | 1376 / 1312 |
| `index_rust` | 12304 / 11056 | 2112 / 1984 | 2160 / 2032 |
| `string_index_field` | 11040 / 9296 | 1696 / 1520 | 1744 / 1568 |
| `string_index_key` | 9824 / 8432 | 1376 / 1264 | 1424 / 1312 |
| `index_string_handler` | 12304 / 9664 | 1888 / 1648 | 1936 / 1696 |
| `string_library_index` | 11872 / 9296 | 1888 / 1520 | 1936 / 1568 |
| `host_get_table` | 10016 / 9120 | 1600 / 1536 | 1648 / 1584 |
| `host_get_table_string` | 10544 / 9152 | 1648 / 1536 | 1696 / 1584 |
| library reads | each 896 less | each 64 less | each 64 less |
| `gsub_pattern_table` | 15616 / 14720 | 3776 / 3712 | 3824 / 3760 |
| `tfor_ipairs` | 10400 / 9504 | 1504 / 1440 | 1552 / 1488 |
| `chain_index` | 1232 / 0 | 192 / 0 | 192 / 0 |
| `chain_newindex` | 1216 / 1168 | 208 / 208 | 208 / 208 |

The library-read row covers `sort_metamethods_read`, `insert_shift_read`,
`remove_read`, `remove_shift_read`, `concat_read`, `move_read`,
`unpack_read` and the other two `gsub_*_table` cycles. `string_library_index` holds
two units per cycle now (its library table continues to a function), where
it held one.

Release frames (`frame_sizes.py`, plain and snapshot alike):
`get_table_with_key_tracking_call` 184 / 120, `index_string` 232 / 120,
`get_field_slow` 488 / 424, `get_field_direct` 456 / 376; the new
`walk_index_tables` (216) and `string_index_handler` (248) have returned
before any handler call. `Frame::run` is unchanged (776, and 824 with
snapshot).

Two intermediate shapes were measured and dropped: a recursive table step
with a pre-guard terminal check grew every release cycle that reaches an
`__index` function through `get_table_with_key_tracking_call` by 96 bytes
(the dispatcher gained a second caller and an outlined frame), and the first
loop, which returned the final handler for its caller to push and call,
grew them by 80 (the returned handler, receiver and unit count stayed live
across the call).

### Register-native slow paths

Measured on plantasjen (x86-64 Linux) on top of df80d81, when the table
and length instructions stopped copying operands above the frame. Each read
is one outlined resolver (`State::resolve_index`, which also absorbed the
string receiver's step and string `__index` handlers, so `index_string` and
its recursion are gone), each store one (`State::store_field`,
`State::resolve_newindex`, which made `__newindex` table chains a loop),
each length one (`State::resolve_length`). They return before any handler
runs; the call is a 24-byte release frame of its own (`index_call`,
`newindex_call`, `len_call`). The before figures are the ones recorded
above for df80d81's code (the `__index` walk follow-up's after column, the
vararg follow-up and the first table otherwise), not a fresh run of that
commit. Cells are **before / after**, bytes per cycle:

| Scenario | Dev, also snapshot | Release | Release + snapshot |
|---|---|---|---|
| `index_field`, `index_method` | 9296 / 7744 | 1520 / 1040 | 1568 / 1008 |
| `index_key` | 8432 / 7712 | 1264 / 944 | 1312 / 912 |
| `index_rust` | 11056 / 9504 | 1984 / 1504 | 2032 / 1472 |
| `newindex_field` | 10192 / 7984 | 1392 / 944 | 1440 / 912 |
| `newindex_key` | 9392 / 7760 | 1360 / 944 | 1408 / 912 |
| `newindex_rust` | 11952 / 9744 | 1856 / 1408 | 1904 / 1376 |
| `length` | 7984 / 7616 | 1168 / 944 | 1216 / 912 |
| `index_string_handler` | 9664 / 7744 | 1648 / 1040 | 1696 / 1008 |
| `host_get_table` | 9120 / 8912 | 1536 / 1344 | 1584 / 1312 |
| `host_call` | 8560 / 8560 | 1296 / 1264 | 1344 / 1232 |
| `tfor_lua` | 7776 / 7792 | 960 / 912 | 1008 / 880 |
| `tfor_host_iterator` | 9136 / 9120 | 1248 / 1200 | 1296 / 1168 |
| `gsub_pattern_function` | 14608 / 14608 | 3584 / 3552 | 3632 / 3520 |
| `gsub_pattern_table` | 14720 / 14512 | 3712 / 3520 | 3760 / 3488 |
| `chain_newindex` | 1168 / 0 | 208 / 0 | 208 / 0 |

Every other release cycle shrinks with `Frame::run`. Release frames
(`frame_sizes.py`): `Frame::run` 776 / 728 (824 / 696 with snapshot);
`get_field_slow` (424), `get_table_slow` (168), `set_field_slow` (216),
`set_table_slow` (184), `set_table_with_key` (200) and `len_slow` (200)
are gone; `get_table_unguarded` 120 / 264, absorbing
`get_table_with_key_tracking_call` (120) and the observed `call_unguarded`
copy (168); `call_from_lua` 120 / 136, the one frame on these paths that
grows (not attributed: it inlines admission, whose only change is the
`FRAME_HEADROOM` term, and an equivalent spelling of that check measured
the same); the
non-reentrant resolvers are `resolve_index` 280, `store_field` 184,
`resolve_newindex` 216, `resolve_length` 136. The dev `tfor_lua` cycle is
16 bytes larger: the iterator call now keeps its call base to deliver
results into the loop registers.

The first build of this shape let the compiler inline the closure, template
constructor and concatenation bodies into `Frame::run` (their buffers,
capture list and runtime handle), which grew it to 808 in plain release.
They are `inline(never)` now; no dispatch cycle pays for them.
