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
