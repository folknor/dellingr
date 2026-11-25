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
  `native_call!("<edge-id>", call)` wrapper naming its edge.
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

With `native-call-trace` disabled, the marker macro expands to exactly the
original call expression. It introduces no block, local, closure or guard:
frames that stay live across a native re-entry are size-sensitive (docs/limits.md,
"Native stack"), and `brokkr test native_bytes_per_level` in the dev and
release profiles must report the same numbers before and after.

The debug-only `native-call-trace` feature records each executed marker in a
thread-local union while an observer is active, and keeps a stack of live
markers using guards around the call expressions. The guard is established
before Rust evaluates the wrapped call expression, including its arguments.
Arguments to a wrapper must not call back into Lua, or a re-entry during
argument evaluation will be attributed to that edge. The lexical inventory
check cannot prove this effect property: tracing whether arbitrary Rust
expressions can reach Lua requires following calls and callbacks, while a
syntax ban on calls in arguments would also reject harmless helpers. Review
argument effects when adding a wrapper. No State field or snapshot data is
added. A release build with the feature is rejected at compile time, as with
the other test instrumentation features. The curated
`native-call-coverage` check shape runs only `native_call_edge_coverage`:
it executes every scenario with all chains and leaves. At each run's first
scenario descent witness, before further cycles, chains or leaves run, it
asserts the live marker stack contains the expanded inventory path in order.
Completed calls cannot satisfy that assertion. Separately, the whole-run
union must contain every non-excluded edge. Its enlarged test stack
is for observation, not evidence for the shipped envelope.

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

Both kinds of entry poll the host interrupt: Lua call admission, and Rust
callback entry at the top of `invoke_rust_fn`, before any
callback bookkeeping changes. A refusal there propagates to the existing call
boundary or dispatch unwind without invoking the callback, so the poll adds
no native call edge or callback frame. What the polls and the library's own
safepoints bound is stated once, in docs/budget.md, "Stopping a script from
the host" (mirrored in `vm/interrupt.rs`).

An edge id is `caller->callee`, with a `:tag` when one caller calls one
callee from several sites (`run_frame->reg_get_field:field` and `...:self`).
Each edge is either covered or excluded:

- covered by the scenarios whose path runs it, or by the `chains` or
  `leaves` it names (the metamethod chain kinds and deepest-point work in
  `tests/native_stack.rs`, for edges that only those reach);
- or `excluded`, with the reason the call can never reach Lua from that site
  (the default sort comparison, a raw `table.move` copy)
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
  `main`. Conversely every `pub fn` on `State` in the closure is a host-api
  root, whether or not it has callers. These are the Lua-reaching entries
  `scripts/poison_contract.py` holds to the `State::guarded` shape
  (`reference/error-reporting.md`, "Panics and poisoning").
- No function in that closure, either execution boundary, `run_invocation`
  or `Frame::run` carries `hotpath::measure`, bare or under the
  `cfg_attr(feature = "hotpath", ...)` form measurement sites use. Its guard
  would remain live across native re-entry and include the nested script's
  work in its time.
- Every call to an inventoried function has exactly one `native_call!`
  wrapper, and the wrapper's caller and callee are the enclosing function and
  the called one. Every wrapper contains exactly one inventoried call, every
  edge id marks exactly one call, and every call site of one pair is tagged
  when there are several. A stray `// native-call:` comment anywhere under
  `src/`, including test-only source files, is an error.
- Every edge is covered or excluded, never both; every chain and leaf an
  edge names exists; every scenario path connects and its native count agrees with the test.
- The scenarios in the inventory are exactly the `SCENARIOS` list of
  `tests/native_stack.rs`, and the chain and leaf names exist in its
  `CHAINS` and `LEAVES` lists.
  Names are read only from direct entry fields in active source text;
  commented-out entries and nested fields do not supply coverage. This is a
  lexical check; the `native-call-coverage` tracing check supplies the
  execution evidence.
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
rustfmt writes them. A wrapper that does not fit on one line is laid out by
rustfmt as `native_call!(`, the edge id and the call on three lines, so the
textlint tripwire accepts `native_call!` on the call's line or up to two
lines above it. The script checks wrapper containment exactly.

Calls through function pointers, closures and trait objects are invisible to
a lexical model, so each kind has a fixed answer. Rust callbacks are entered
only through `invoke_rust_fn`, which is a boundary. A closure handed to a
helper is attributed to the function that wrote it. A host closure
(`with_restricted_env`, the snapshot loader's setup) is host code, which
reaches Lua through the `host-api` roots. The table sort's heap runs over a
`HeapSortOps` trait whose `heap_less` is called through a generic receiver,
so the script resolves `ops.heap_less(` to every method of that name; only
`LuaSortOps::heap_less` reaches Lua, and the number and string backends'
methods of the same name never do. The `TableSortAccess` implementations
under `LuaSortOps` read and write raw. A new indirect call
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
   it is entered from outside), the call site to `[[edge]]`, and wrap the
   call in `crate::native_call!("<edge-id>", call)`. Keep the argument
   expressions free of calls that can re-enter Lua, since the tracing guard
   is live while those expressions are evaluated.
2. If the edge opens a new path, add a `Scenario` to `tests/native_stack.rs`
   that recurses through it (its `native` and `meta` per cycle, a `DESCEND`
   inside the handler the path calls, and Lua checks of any returned value),
   its name in the `scenario_tests!` list there (one test per scenario), and
   a `[[scenario]]` with the same name and its path. If only a chain
   kind or a deepest-point item reaches it, name those on the edge instead.
3. If the call can never reach Lua, or is outside the envelope, mark the
   edge `excluded` with the reason.
4. Run `brokkr test native_bytes_per_level`, whose sweeps cover both
   profiles, to see the new path's bytes per level, and `brokkr check`, which runs the
   envelope in both profiles. Add the new row to the table below when you
   re-measure it (see "Current native bytes per level").

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

## Current native bytes per level

The recurring native stack of each scenario's cycle, as printed by the
on-demand probe `brokkr test native_bytes_per_level`, which covers dev and
release through the sweeps configured in `brokkr.toml`. Measured on the
commit that builds short library string results in stack storage, on top
of c1667e8, on bygg (x86-64 Linux) with
`rustc 1.102.0-nightly (65ce8611f 2026-11-18)` and that host's global Cargo
configuration. Against c1667e8 measured the same day, that change moved
release `tostring` down 48 bytes and the other tostring rows and `format`
down 16 to 32, and `concat_*` up 48 (`table.concat` holds its separator
copy, a stack array when short, across `__index`); `gsub_*` moved by 16
with no gsub source change. Release is Cargo's default release profile
(the repository sets no overrides). Dev's default and snapshot builds
agree in every row; the release snapshot build differs from release
default in the `error_tostring` and `gsub_pattern_*` rows.

These are compiler-dependent measurements of one build, not limits and not
assertions of headroom for another build; a different compiler version or
flag set can move them. docs/limits.md, "Envelope", states what they are
checked against. Re-measure and replace the table (commit, host, `rustc -V`
together) when a change moves a frame on these paths; the measurement history
that led here is in [performance.md](performance.md).

Cells are bytes per scenario cycle. `index_rust` and `newindex_rust` take two
native levels per cycle; the others take one. The two `chain_` rows measure a
single metamethod table step and cost no native stack. Transient
deepest-point work is excluded; the full-depth envelope tests cover it
separately.

| Scenario | Dev, also snapshot | Release | Release + snapshot |
|---|---|---|---|
| `index_field` | 10592 | 624 | 624 |
| `index_method` | 10592 | 624 | 624 |
| `index_key` | 10704 | 736 | 736 |
| `index_rust` | 12896 | 1104 | 1104 |
| `newindex_field` | 11040 | 736 | 736 |
| `newindex_key` | 10608 | 704 | 704 |
| `newindex_rust` | 13344 | 1216 | 1216 |
| `length` | 10448 | 496 | 496 |
| `string_index_field` | 10592 | 624 | 624 |
| `string_index_key` | 10704 | 736 | 736 |
| `index_string_handler` | 10592 | 624 | 624 |
| `string_library_index` | 10592 | 624 | 624 |
| `host_get_table` | 12448 | 1008 | 1008 |
| `host_get_table_string` | 12448 | 1008 | 1008 |
| `host_set_table` | 12304 | 992 | 992 |
| `host_call` | 11984 | 944 | 944 |
| `host_anchor` | 12576 | 1120 | 1120 |
| `host_invoke` | 12544 | 976 | 976 |
| `host_tostring_bytes` | 13232 | 1328 | 1328 |
| `host_to_string_with_meta` | 13408 | 1376 | 1376 |
| `tostring` | 12368 | 1120 | 1120 |
| `print` | 12864 | 1328 | 1328 |
| `format` | 13728 | 1408 | 1408 |
| `error_tostring` | 13056 | 1424 | 1440 |
| `sort_comparator` | 14016 | 1456 | 1456 |
| `sort_comparator_right` | 14016 | 1456 | 1456 |
| `sort_comparator_sortdown` | 14016 | 1456 | 1456 |
| `callable_rust` | 14208 | 1536 | 1536 |
| `tail_rust` | 12192 | 928 | 928 |
| `tail_callable_rust` | 12384 | 1008 | 1008 |
| `sort_metamethods_comparator` | 13728 | 1568 | 1568 |
| `sort_metamethods_length` | 11984 | 1136 | 1136 |
| `sort_metamethods_read` | 13184 | 1328 | 1328 |
| `sort_metamethods_write` | 13040 | 1312 | 1312 |
| `insert_length` | 12224 | 1184 | 1184 |
| `insert_shift_read` | 12512 | 1184 | 1184 |
| `insert_shift_write` | 12384 | 1168 | 1168 |
| `insert_value` | 12384 | 1168 | 1168 |
| `remove_length` | 12192 | 1120 | 1120 |
| `remove_read` | 12480 | 1120 | 1120 |
| `remove_shift_read` | 12480 | 1120 | 1120 |
| `remove_shift_write` | 12352 | 1104 | 1104 |
| `remove_clear` | 12352 | 1104 | 1104 |
| `concat_length` | 12720 | 1184 | 1184 |
| `concat_read` | 13008 | 1184 | 1184 |
| `move_read` | 12800 | 1248 | 1248 |
| `move_write` | 12672 | 1088 | 1088 |
| `unpack_length` | 12144 | 1072 | 1072 |
| `unpack_read` | 12432 | 1072 | 1072 |
| `gsub_empty_function` | 13216 | 1584 | 1584 |
| `gsub_plain_function` | 13072 | 1552 | 1552 |
| `gsub_pattern_function` | 16480 | 3216 | 3184 |
| `gsub_pattern_captures` | 16480 | 3216 | 3184 |
| `gsub_empty_table` | 13344 | 1504 | 1504 |
| `gsub_plain_table` | 13200 | 1472 | 1472 |
| `gsub_pattern_table` | 16608 | 3136 | 3104 |
| `tfor_lua` | 10672 | 656 | 656 |
| `tfor_ipairs` | 12528 | 1056 | 1056 |
| `tfor_host_iterator` | 12384 | 944 | 944 |
| `chain_index` | 0 | 0 | 0 |
| `chain_newindex` | 0 | 0 | 0 |

The largest recurring cycle is `gsub_pattern_table` in dev and
`gsub_pattern_function` (and its equal `gsub_pattern_captures`) in both
release columns. No metamethod chain step costs native stack in any setting.
