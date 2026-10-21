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
ids of rebuilt strings); decided not to be a cross-version contract
(`docs/determinism.md`), so it ships as a changelog-called-out change. Enforceable by a text rule banning `alloc_string("__` /
`"__[a-z]+"` literals outside the enum's module.

## Heap, tables, persistence

## Stdlib and patterns

## Host surface and CLI

## Tooling, tests and harnesses
