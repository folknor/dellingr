# Bugs

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

Defects turned up by the hygiene hunt at 6c795d1 (six Opus hunters: compiler,
VM core, heap/tables/persistence, stdlib/patterns, host surface, tooling). The
hunters read code and ran nothing; every entry is a claim to verify, not a
verified fact. Hygiene findings are in `notes/hygiene.md`.

## BUG-001 - The ipairs iterator has no registered id, so a reachable one fails the save

Source: stdlib hunter (found by reading, not run).

`base_ipairs` pushes `base_ipairs_iter` with `push_rust_fn`, not `push_lib_fn`,
and nothing calls `register_lib_fn_id` for it. `pairs` returns `base_next`,
registered as `"next"`, and gmatch registers its iterators up front for exactly
this reason. `docs/save-state.md` says an unregistered reachable `RustFunc` fails
the save. So a script that keeps the iterator (`local it = ipairs(t)` captured
as an upvalue or stored in a global, or a save taken while a generic `for` over
`ipairs` is suspended in a host callback) cannot be saved.

Fix: `register_lib_fn_id("ipairs.iter", base_ipairs_iter)` in `open_base`, and
`push_lib_fn` in `base_ipairs`. A test that saves a State holding an `ipairs`
iterator pins it.

## BUG-002 - `brokkr test` can silently rewrite the save golden fixture

Source: tooling hunter.

`tests/save_golden.rs` has an ignored test,
`deliberately_rewrite_snapshot_fixture_file`, that overwrites
`tests/fixtures/save_golden_current.bin`. `brokkr test <NAME>` always passes
`--include-ignored`, and `<NAME>` is a substring filter, so any filter matching
the name (`save_golden`, `snapshot`, `fixture`, `rewrite`, ...) runs the
regeneration. In the same sweep the golden comparison then checks against the
freshly written bytes, so the format-drift guard defeats itself. The only trace
is a dirty `.bin` in `git status`. The ignore attribute was the only safety, and
the tool removes it.

Fix: take regeneration out of the test harness: an env-gated helper that panics
without an explicit variable (e.g. `DELLINGR_REGENERATE_GOLDEN=1`), or a small
`examples/`/xtask binary. Enforcement: a rule that no test writes into the
source tree, backed by a text check for `fs::write` outside
`CARGO_TARGET_TMPDIR` in `tests/`.

## BUG-003 - Hotpath harness warm timing includes FIFO writes and backpressure sleeps; retry loop is unbounded

Source: tooling hunter.

In `examples/hotpath.rs` the warm phase emits `WARM_BLOCK_START`/`WARM_BLOCK_END`
inside the `warm_start..warm_total_us` window. Each emit is a FIFO syscall, and
on `WouldBlock` `write_fifo_all` sleeps 1ms in a loop, charged to `warm_avg_us`,
the verdict metric. With at most 32 blocks the syscall cost is small; a stalled
reader turns it into whole milliseconds of fake regression. The retry loop has
no deadline: a reader that keeps the FIFO open but stops draining hangs the
benchmark forever.

Fix: time block bodies only and sum them, or buffer markers and flush outside the
timed region; give the retry loop a deadline after which the run fails.
Enforcement: structural, a single timed closure containing no emit calls.

## BUG-004 - `DELLINGR` env leaks into benchmarks and mislabels them

Source: tooling hunter.

`scripts/lib.sh::dellingr_bin` returns `$DELLINGR` whenever it is set. That is
the hook `diff_test.sh` needs, but `bench.sh` and `hotbench.sh` call the same
function. With `DELLINGR` still exported, `bench.sh` times that binary while its
header prints `commit: $(git rev-parse HEAD)` and a dirty count, claiming a
provenance it does not have. In `hotbench.sh` the harness is always rebuilt at
HEAD while the hyperfine half uses whatever `DELLINGR` names, so the two halves
measure different binaries.

Fix: two lib.sh functions, an always-build one and an override-honouring one,
with only `diff_test.sh` using the override; or have `bench.sh` print the binary
path and refuse when it is not the one it just built.

## BUG-005 - `DELLINGR_DIFF_TIMEOUT` is validated at use, once per file

Source: tooling hunter.

With GNU `timeout`, a malformed value (`5 s`, `5sec`) makes every invocation exit
125, reported as `FAIL: <name> (dellingr exited 125)` for every file in the
corpus, which blames dellingr. With the Python fallback each file prints
`invalid DELLINGR_DIFF_TIMEOUT` and fails. The operator sees N false failures
instead of one refusal at startup.

Fix: parse once before the loop (the Python parser already exists) and exit 2 on
garbage. Enforcement: a test running `diff_test.sh` with a bad value and
expecting a single refusal line.

## BUG-006 - `println!` panics on a closed pipe; the CLI exits 101, contradicting docs/cli.md

Source: host-surface hunter.

`DefaultCallbacks::on_print` (the trait default body), `main`'s
`print!`/`println!` and every `eprintln!` panic with "failed printing to stdout"
on EPIPE. `dellingr script.lua | head -1` on a script printing more than one line
aborts with a Rust panic and exit 101, while `docs/cli.md` says every failure
exits 1. For an embedder using the default callbacks in a service whose stdout is
closed, this is a process abort from inside the VM on host-controlled
conditions.

Fix: write with `writeln!(io::stdout().lock(), ..)` and treat an error as
terminal (exit quietly, or let `on_print` return `Result` so the VM can fail the
callback). Related hygiene: the default-body issue in `notes/hygiene.md` (the
`on_print` default entry). Testable with a CLI test piping into a closed reader.

## BUG-007 - `Display for Error` ends with a newline when a traceback exists

Source: host-surface hunter.

`impl Display for Error` uses `writeln!` for every traceback frame, so the string
ends in `\n`. The CLI's `eprintln!("Error: {error}")` then prints a blank line
after every runtime error, and any host that appends its own newline gets the
same. Fix: join frames with `\n`, no trailing one. Testable.

## BUG-008 - Unbounded names at compile and registration make saves fail later

Source: heap/tables hunter and host-surface hunter (same mechanism from both
sides).

`save_state.rs` caps several byte fields (`MAX_SAVE_IDENTIFIER_BYTES` 4 KiB,
`MAX_CHUNK_NAME_BYTES` 4 KiB, `MAX_CHUNK_SOURCE_BYTES` 256 KiB, among others),
but nothing upstream enforces them:

- `register_rust_fn` accepts an id of any length; an id over 4 KiB registers fine
  and makes every later `save_state` fail with `TooLarge { what: "function id" }`.
- `Engine::compile_named` / `State::load_string_named` accept a source name of any
  length (the CLI passes the whole filename); over 256 KiB, saves fail with
  `TooLarge { "chunk source" }`. `MAX_CHUNK_SOURCE_BYTES` is also misnamed: it
  caps the chunk's source *name* (`ByteField::ChunkSource`), not its text.
- `Bytecode.name` (derived from source identifiers, bounded only by
  `MAX_SOURCE_BYTES`) over 4 KiB fails saves with `TooLarge { "chunk name" }`. A
  script can make its own host unable to save by writing
  `function aaaa...aaaa() end`.

Related growth: each chunk and each `Error`'s context holds an `Arc<str>` of the
source name, and `StackFrame.source` is a fresh `String` per frame per error, so
a long name times `MAX_CALL_DEPTH` frames is copied per error.

Fix: move the codec limits into `limits.rs` (see the limits-home entry in
`notes/hygiene.md`); enforce id length in `register_rust_fn` and source-name
length (`limits::MAX_SOURCE_NAME_BYTES`) at the compile/load entry points; cap or
truncate `Bytecode.name` in the parser (diagnostic only); make
`StackFrame.source` an `Arc<str>`. Enforceable by test: compile/register at
limit+1 fails, and anything compilable is savable.

## BUG-009 - `table.move` cost on the error path depends on whether a budget is configured

Source: stdlib hunter.

With a budget, `table.move` charges 1 before each element; without one, it
charges the full count up front. If a raw `move_element` fails partway (stack
cap, for example), `cost_used` differs between the two modes for the same
script: a host flag unrelated to the script changes observable `cost_used`.
`find_subslice` / `append_bytes_units` already use `consume_units` for batching
with exact one-at-a-time semantics; `table.move` should too and drop the branch.
Enforcement: a budget-sweep test like the `find_subslice` one.

## BUG-010 - CLI accepts `-` as a positional, then reads a file literally named `-`

Source: host-surface hunter.

`-` is excluded from the unknown-option check (the conventional stdin spelling)
but is then read as a path. Either implement stdin or reject `-` with a message;
`docs/cli.md` says nothing about it.

## BUG-011 - CLI test `ScratchDirectory` fails on a leaked directory from a recycled pid

Source: tooling hunter.

CLI tests name scratch paths `dellingr-cli-<name>-<pid>` under
`CARGO_TARGET_TMPDIR`. `ScratchDirectory` uses `create_dir`, so a directory leaked
by a killed run whose pid is later reused makes the test fail with "should be
created". Fix: remove first, or add a monotonic or random suffix.

## BUG-012 - `-- DIFF:` files still run both references, and a reference timeout fails the gate

Source: tooling hunter.

For `has_diff=1` files `diff_test.sh` runs lua5.2 and lua5.4 and only then skips
the comparison. A reference timeout on a DIFF file (a dellingr-only construct
that loops in reference Lua) fails the gate although the file is declared
divergent, while reference non-zero exits are ignored everywhere, so the policy
differs by failure kind. The marker grammar is also inconsistent: `SKIP-DIFF`
requires a reason (`'^-- SKIP-DIFF: .'`), `DIFF` does not (`'^-- DIFF:'`).

Fix: skip reference runs for DIFF files; require a reason for both markers.
Enforcement: a test grepping the corpus for empty-reason markers.

## BUG-013 - `bench.sh` calls python3 before checking for it

Source: tooling hunter.

The default script list is computed with `python3 -c` before the tool loop that
reports `missing tool: python3`. Without python3 the result is "no benchmark
scripts found" plus a shell error instead of the clean message.
