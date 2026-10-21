# Hygiene findings

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

Consolidated from the hygiene hunt at 6c795d1: six Opus hunters, one per scope
(compiler front end, VM execution core, heap/tables/persistence, stdlib and
patterns, host surface/cost/CLI, tooling and harnesses). The question was what
has more than one owner, or rests on an assumption about its host. Hunters read
code and ran nothing. Entries are unranked and unverified; where several hunters
reported one mechanism, that is noted as a fact about the reports, and where they
disagreed both readings are kept. The defects found along the way have all been
fixed or deferred; deferred items are in `notes/deferred.md`.

Common context several entries lean on: the build can enforce clippy lints and
`clippy.toml` path bans, visibility, types, golden tests, and the gremlins scan.
It has no custom text rule beyond gremlins and no module dependency-direction
rule, so many "enforce by grep" proposals below also imply adding such a check
to `brokkr check`.

---

## Encodings and operands

## HYG-005 - Snapshot environment token names spelled at more than one site

Reported by the VM-core hunter.

`capture_env_tokens` names one token through `STRING_METATABLE_TOKEN` and another
as the inline literal `"_G.metatable"`; builtin tokens come from `slot.name()`;
the loader in `save_state` must spell the same strings. Fix: one token table
owned by save_state.

---

## Limits, tunables and where they live

## HYG-006 - No single home for limits and tunables

Reported by the stdlib, host-surface, heap and VM-core hunters.

`limits.rs` claims to be the home of fixed resource and encoding limits and says
the snapshot format validates against them, but replay-visible limits live
elsewhere:

- `patterns/luapat.rs`: `MAXCCALLS = 200` (`MatchDepthExceeded`),
  `LUA_MAXCAPTURES = 32`.
- `string_format.rs`: `MAX_FORMAT_LEN = 22` with no note that it is lstrlib's
  `MAX_FORMAT - 10`; width/precision digit cap is a bare `> 2`.
- `basic.rs`: `tonumber` base range `2..=36` inline.
- `vm/table_ops.rs`: the shared sort's `len >= i32::MAX` "array too big" rule.
- `vm/object.rs`: GC policy defaults.
- `compiler/parser/table.rs`: inline-template threshold `4` (HYG-016); the
  doubled syntax-level charge per call/index suffix (HYG-017); `numeral.rs`
  `MAX_SIGNIFICANT_HEX_DIGITS` and the `+2048` exponent clamp (correctness
  parameters, not tunables, but undocumented as such).
- `vm.rs` capacity hints (HYG-007) and `Vec::with_capacity(256/64)` in
  `empty_with_callbacks`; `HOST_GLOBAL_CAPACITY_HEADROOM`.

Conversely `limits.rs` holds non-limits as `pub`: `COST_CHECK_INTERVAL` (a
batching knob `set_cost_budget` promises is unobservable) and
`GMATCH_PATTERN_CACHE_*`, advertising internals as contract.
`Frame::COST_CHECK_INTERVAL` re-aliases the limits constant with its own
docstring (VM-core and host hunters). Three independent `200`s
(`MAX_SYNTAX_DEPTH`, `MAX_METAMETHOD_DEPTH`, `MAXCCALLS`) look related and are
not; `MAXCCALLS` is reference-pinned. The VM-core hunter judged `limits.rs`'s "no
injection point" rationale (replay contract) a real answer, not a finding.

Fix: `limits` (public, script-observable caps, everything that produces an
`ErrorKind`, with a "reference-pinned library limits" heading) plus a
`pub(crate) tuning` module (batch interval, cache sizes, GC thresholds, capacity
hints). Drop the `Frame` alias. Enforceable: the module split itself; not by
lint.

## Cost model

## Compiler front end

## VM execution core

## HYG-037 - Metamethod event names interned by string literal at each dispatch site, with three error/allocation policies

Reported by the VM-core and heap hunters.

`"__index"` (`metamethod.rs` x2, `eval_index.rs` x2, `vm.rs`, `basic.rs`),
`"__newindex"` (`metamethod.rs`, `basic.rs`), `"__call"` (`eval.rs`),
`"__len"` (`eval_store.rs`, `table.rs` stdlib), `"__tostring"` (`table_ops.rs`
twice, once `b"__tostring"` for `find_string`, once for `alloc_string`),
`"__metatable"` (`basic.rs` x2). A typo compiles and never dispatches. Each
alloc site does its own rooting dance; error policy differs (`?` in
`metamethod.rs`/`index_string`, `.expect(..)` in `eval.rs`, `instr_length`,
`protected_index_key`), and `string_tostring_handler` probes with the
non-allocating `find_string` while `metatable_tostring_field` allocates (may GC).
Every metatable miss on a read allocates and may collect.

Fix: a `MetaEvent`/`MetaName` enum with names interned once per State as roots in
`mark_gc_roots`, read as `meta_names[MetaName::Index]`: removes dispatch-time
allocation, rooting dances and the expect/`?` split. Changes GC timing (and `%p`
ids), deterministic but snapshot-visible, so call it out; see HYG-038 on whether
timing is contract. Enforceable by a text rule banning `alloc_string("__` /
`"__[a-z]+"` literals outside the enum's module.

## HYG-039 - `InternalError(String)` assembled ad hoc; `InvalidJump` is a second "VM bug" kind

Reported by the VM-core and host-surface hunters.

About 20 VM sites write free text ("SetList(0): no constructor base", "call-base
marker is below the active frame", "unknown opcode: N", "InitField: expected
table, got {typ}"); none carries opcode, ip or chunk as data; `locate_in_frame`
adds a line only. `ErrorKind::InvalidJump { ip, offset }` also means "compiler bug
or corrupt bytecode" and hand-copies the `"internal error: "` prefix. Fix:
`ErrorKind::VmInvariant { opcode, ip, what: &'static str }` (or
`InternalDetail::InvalidJump {..}` etc.) with one prefix owner, filled at the
frame boundary. Enforceable by type.

## HYG-040 - VM panics on bytecode-controlled conditions; defensive checks applied inconsistently

Reported by the VM-core hunter.

Safe only because the verifier ran, with nothing in the types saying so:
`OP_SWAP` `stack.swap(len-1, len-2)` underflows; `OP_DUP` `.expect`; `pop_val`
`expect("VM bug")` (OP_POP, OP_EQUAL, every binary op); unchecked
`stack[base + n]` in `instr_set_local`/`get_local`/`tfor_*`; unchecked
`frame.upvalues[idx]`, `bytecode.nested[i]`, `number_literals[i]`,
`string_literals[n]`. If the snapshot load path ever skips verification for a
chunk (not verified), a corrupt save becomes a host panic where `LoadError` was
owed. Meanwhile `Frame::jump` and `instr_set_list_count` keep defensive checks "so
a verifier regression returns a VM error". The unreachable
`"fused branch dispatched with a non-fused opcode"` and `"unknown opcode"` errors
belong to the same question. Fix: a `VerifiedBytecode` newtype only the verifier
constructs, `Frame` accepts only that; then drop defensive checks everywhere or
keep them everywhere. Enforceable by signature.

## HYG-041 - `Val` is `Eq` and `Hash` although NaN is not reflexive

Reported by the VM-core and heap hunters; narrowed in wave 4 (`Val::hash` is
total, `to_bytes_with_heap` returns `Option`, `format_pointer_id` is total, map
slots are a `TableKey` newtype, and `live_hash_entries` skips dead slots).

`Val` still implements `Eq`, and `Val::hash` maps NaN to a fixed token, so a NaN
used as a key in any `Val`-keyed map (the pointer-id index is one) is stored but
never found. No known path reaches it today. `TableKey::new` also repeats the
`Val` to `TableKeyInput` mapping that `save_state.rs::runtime_table_key` does
(see HYG-051). Fix: key the pointer-id index by a pointer-only type and drop
`Hash`/`Eq` from `Val`.

## HYG-044 - Call-site recording and cost flushing placed by hand per opcode

Reported by the VM-core hunter.

`record_call_site` is called before `OP_CALL`, `TFOR_CALL`, `LENGTH`,
`GET_FIELD`, `GET_TABLE`, `SET_FIELD`, `SET_FIELD_AT`, `SET_TABLE`: any opcode
that can re-enter Lua. `flush_local_cost` is called by hand in each metamethod
helper (`metamethod.rs` x4, `eval_index.rs`, `eval_store.rs`). A new opcode that
forgets either gets a wrong traceback line or a `cost_used` that depends on the
batch interval (replay-visible). Fix options: `State::call` flushes itself; record
the ip into `CallInfo` unconditionally on every call out (one store); or an
`Instr::may_reenter()` table cross-checked by a test. Enforceable by a test running
each opcode against a metatable'd operand asserting traceback line and
`cost_used`.

## Heap, tables, persistence

## HYG-051 - Val-to-table-key mapping written three times; nil/NaN refusal written five times

Reported by the heap hunter; narrowed in wave 6 (`Val::table_key` is the one
runtime mapping, used by table storage, raw assignment and snapshot keys).

Still open: `Val`'s `Hash` numeric arm calls `normalize_table_key` through
`TableKeyInput::Number` directly, which keeps `TableKeyInput` visible outside
`table.rs`; `verify::resolve_key` keeps its own mapping for `SavedVal`; and
`Table::hash_get` / `get_with_index` now normalize the whole key just to refuse
nil and NaN, where two pattern checks used to suffice (table read path; worth a
cheap `is_valid_table_key`).

## HYG-052 - gmatch matcher ordering invariant and env-token schema have no structural owner

Reported by the heap and stdlib hunters; narrowed in wave 1 (ids and keys now
live in `src/gmatch_schema.rs`, reads are strict, the dead branch is gone).

- Ordering invariant: `gmatch_subject_matcher_and_cost_meter` depends on
  `memoize_gmatch_pattern` running immediately before with no allocation between,
  guarded by an `InternalError`; a single `with_gmatch_matcher(.., |subject,
  matcher, meter| ..)` would make it structural.
- The `_G` proxy metatable and string metatable are likewise library-installed
  state the VM tracks as env tokens without a schema owner.

- No test saves and reloads a live gmatch iterator.

Fix: `with_gmatch_matcher`; a schema owner for the env tokens; the save/reload
test.

## HYG-053 - Table and snapshot errors shed their location and reason

Reported by the heap hunter; narrowed in wave 6 (table materialization failures
keep their reason through `LoadError::InvalidTable` with a typed
`TableLoadLocation`; runtime locations are already filled by `locate_in_frame`).

Still open: the verifier's `LoadError::CorruptArena { reason, index }` does not
say which arena the index belongs to (env-delta checks report `key_index` or
`delta_index` inconsistently, `validate_metatable` reports `0`), and
`table.rs` still depends on `Error`. Fix: a typed location on `CorruptArena`;
`Table` returns its own key error and the State layer wraps it.

## HYG-054 - Snapshot in-flight inventory and load-time reset are two hand-maintained lists

Reported by the heap hunter.

`State::validate_quiescent` enumerates 12 fields that must be empty;
`materialize_payload` clears 14 (`transient_roots.values` and `.suspended_envs`
separately where the validator calls `is_empty()`); `State::new` is a third
writer. The validator's comment asks to keep the inventory updated by memory. Fix:
an `InFlight` struct with `is_quiescent()` and `reset()`, destructured without
`..` so a new field fails to compile. Enforceable by type.

## HYG-059 - Snapshot encoder holds a breadcrumb per encoded value

Reported by the heap hunter.

`SaveBuilder::breadcrumbs` pushes a `Breadcrumb` (a `String` for globals) per
key, value, metatable and upvalue, only so `UnregisteredFunction` can render a
path: a second copy of the graph's size for the whole save. `encode_env_deltas`
pushes a fresh root breadcrumb with a newly truncated token copy per key and
value. Fix: record nothing on the happy path; on failure re-walk from roots. Also
`SaveBuilder::push_path` truncates every `PathSegment::Global` to
`MAX_PATH_GLOBAL_BYTES`, and all eight call sites (`finish`, `encode_env_deltas`
x7) already truncate before constructing; delete the call-site copies and make
`PathSegment::global(&str)` the only constructor.


---

## Stdlib and patterns

## Host surface and CLI

## Tooling, tests and harnesses
