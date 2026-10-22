# Deferred findings

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

Entries moved out of the bug and hygiene documents (hunt at 6c795d1)
before the resolution loop, judged on their face rather than verified. They keep
their IDs; cross-references to other IDs point back into those two documents.
Entries are moved whole, so some carry a small part that is fixable as written;
the category names the part that kept them out of the loop.

## Approved, not yet implemented

The user walked through these one by one on 2026-10-22 and approved each
decision below. Entries are removed as they ship.

They land together as one breaking release, so the game host migrates once:

- HYG-079 and HYG-035 change the public host API.
- The snapshot format is already bumped to 10 in this release (the
  cache-operand unification). If HYG-079's chunk names are persisted, that
  change rides on the same unreleased bump; do not bump again.
- The cost model is already bumped to 6 in this release (gmatch's literal
  `^`).
- CHANGELOG.md calls out each replay-visible change.

## HYG-079 - Host boundary assumes UTF-8: lossy `print`, UTF-8-only source

Reported by the stdlib and host-surface hunters.

`print` converts to `&str` lossily because `HostCallbacks::print` takes `&str`;
scripts can print arbitrary bytes and the host sees U+FFFD, and the 3x expansion
preflight in `print` and `error` exists only because of it. `load_string(&str)` is
the only load API, so the CLI's `fs::read_to_string` refuses non-UTF-8 sources
(a clean refusal naming the path; stdin input is refused the same way) although
Lua strings are bytes; non-UTF-8 paths become the chunk name via
`to_string_lossy`, silently degrading the identifier.

Decision: the full fix, with errors typed properly.

- `on_print` takes the message as `&[u8]`.
- A byte source load, which becomes the single compile entry point (see
  HYG-035); the CLI uses it for files and stdin.
- Chunk names are byte-exact, or follow one stated conversion rule.
- `ErrorKind::ScriptError` carries a typed payload instead of
  `message: String`: nil, boolean, number, or string as exact bytes. Values
  that cannot outlive the State's GC (tables, functions, ...) carry their type
  plus their `__tostring` rendering as bytes. `error()` with no argument is
  the nil payload, not a made-up message. `Display` renders today's text, so
  CLI output for valid UTF-8 is unchanged.
- The remaining `ErrorKind::RuntimeError(String)` and `InternalError(String)`
  construction sites (`src/compiler.rs`, `src/compiler/parser.rs`,
  `src/vm_aux.rs`, `src/error.rs`) get typed variants or reasons, as the
  library and pattern errors already have.
- The 3x preflight goes from `print` and `error`. `State::to_string` keeps
  its checked lossy conversion: it promises a `String`.
- Replay-visible: invalid UTF-8 in printed text and error values reaches the
  host as the script's bytes instead of U+FFFD.

## HYG-035 - Thin wrapper layers and a facade with one bool of payload

Reported by the compiler and host-surface hunters.

`Parser::describe_token_type` returns `typ.spelling()`; `parse_fndef`,
`parse_fndef_named`, `parse_fndef_method` forward to `parse_fndef_with_params`;
two layers of `parse_str`/`parse_str_named` (`compiler::` wraps `parser::`).
`parser::parse_str` exists separately only so tests can see pre-`finalize`
output. On the host side, `Engine::compile`/`compile_named` never reference
`self` (Engine's only state is `install_stdlib: bool`); `analyze_cost(source)` and
`Engine::analyze_cost(&program)` are two entries to one thing;
`State::load_string(_named)` is a third compile path.

Decision: collapse the internals and drop `Engine`. One internal
`compile(source, name)`, with a test helper for pre-finalize output; the
parser forwarders and `describe_token_type` fold away. Publicly,
`Program::compile` plus `State::new`/`raw` replace `Engine`, and one
`analyze_cost` remains. The byte source load from HYG-079 is the one compile
path, not a byte variant beside each of today's three.

## HYG-077 - Small stdlib duplications and unreachable checks

Reported by the stdlib hunter.

- `tostring` and `string.format("%s")` each implement "a string bypasses
  `__tostring` unless the shared string metatable has one"; `error` implements the
  opposite (always bypass). One `State::tostring_bytes(idx)` should own the first.
- Lua-index normalisation: `string.rs` has `lua_start_index`/`lua_end_index`,
  `table.rs` has per-function rules (matching 5.4's quirks), `select` its own.
  The rules really differ, but nothing documents them in one place.
- `_G.__index`/`__newindex` handlers check args 2 and 3 via `check_any_named`;
  the VM always passes them and scripts cannot reach the handlers (`__metatable =
  false`), so the checks are unreachable from Lua (a host could call them). Noted
  so nobody cites them as precedent.

Decision: split it up. The first bullet lands with HYG-079, whose byte-exact
`error` and `print` rework the same conversion: `tostring_bytes` owns the
rule, with `error`'s opposite rule beside it. The second gets one short
section listing the per-function index rules (`reference/` or a comment
block in `src/lua_std.rs`). The third gets a comment at the `_G` handlers.
Then the entry is removed.

## Unmeasured performance

## HYG-045 - Error-path cleanup decided per site

Reported by the VM-core hunter; the panic-safety half shipped as the poison
flag. Remaining, as of the call flattening (bc37227, 27ee714):
`truncate_call_frame` restores the frame from several sites in
`src/vm/eval.rs` (`call_unguarded`, `fail_call`, `invoke_rust_fn`), and
`handle_index_metamethod_inner` and `index_string` truncate temporaries by
hand. A guard or `frame_scope` helper would make "every exit restores"
structural. Not done because it cannot be a Drop guard (the guard would hold
`&mut State` while the body uses it), so it is either a closure helper - the
frame bloat `call_unguarded` already rejected for `with_rooted_value`, and it
is still on the native re-entry path that `MAX_NATIVE_CALL_DEPTH` is sized
for - or a plain struct that every `Ok` exit must disarm by hand, which keeps
the per-site count it was meant to remove. Revisit only with a
native-stack-per-level measurement of `call_unguarded` showing the closure
form is frame-neutral.

Decision (2026-10-22): deferred; stays here.
