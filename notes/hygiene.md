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

## Limits, tunables and where they live

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

## Heap, tables, persistence

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

## HYG-053 - `table.rs` still returns the crate `Error`; gmatch corruption errors name the closure

Reported by the heap hunter; narrowed in waves 6 and 7 (every snapshot
corruption error carries a typed location).

Still open: `Table::insert`, `set_list`, `rebuild*` and
`clear_and_insert_entries` return `Result<(), Error>`; a table-local key error
would need wrapping at about 11 caller sites (a comment above the import says
so). Separately, most gmatch corruption errors in `verify.rs` report the
closure's index under `Objects`, not the gmatch state table's.

## HYG-105 - Env token aliasing depends on schema order held by a comment

Reported by the wave 7 reconciler.

`capture_env_tokens` classifies aliased tables by the last captured token, so
`BUILTIN_ENV_TOKENS` order is a schema invariant held only by a comment. A test
that two builtin slots aliasing one table resolve to the same token on save and
load would pin it.

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
