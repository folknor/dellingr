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
