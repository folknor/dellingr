# Defect hunt findings

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

Coverage: the `frontend`, `dispatch`, `heap`, `snapshot`, `host`, `stdlib`,
`cost` and `determinism` scopes were each read by one codex hunter
(gpt-6.1-sol, high effort). `cost` and `determinism` reported no confirmed
defects. The `codegen` scope (`src/compiler.rs`, `src/compiler/codegen.rs`,
`src/compiler/verify.rs`, `src/instr.rs`, `src/disassemble.rs`) was not hunted:
its run was stopped by the provider's safety filter and it produced no report.
Its one surviving progress note, explicitly unconfirmed by the hunter, was: the
verifier checks register bounds but does not establish that stack-shrinking
instructions preserve slots held by open upvalues, so edited snapshot bytecode
might bypass the compiler's convention of keeping call and open-vararg windows
above live locals. The hunter was tracing a minimal case through return cleanup
when the run stopped.

## BUG-004 - `RawField` promises integer implementations it does not provide

Hunter label: P3, from both `heap` and `host`, reported independently. Site:
`src/vm/raw_field.rs` (reported at lines 47-49).

Its public documentation says it supports "every fixed-width integer type."
Implementations cover only signed and unsigned 8-, 16-, 32- and 64-bit integers;
`i128` and `u128` are absent. Consequently, `state.raw_field::<i128>(1, "value")`
fails to compile even for a value such as `1`, as do the `u128` and `Option`
forms. The sealed trait prevents callers from supplying the missing conversion.

Hunters' suggested directions: `heap` says either enumerate the supported types in
the documentation or fulfill the existing promise. `host` says listing the
supported widths explicitly suffices and adding types is unnecessary to resolve
the documentation defect.
