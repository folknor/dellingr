# brokkr-side findings

Findings from the verification hunt whose fix lies in brokkr's behaviour
rather than in dellingr's code, or where dellingr's workaround depends on what
brokkr offers. Each entry names the scopes whose hunters reported it and, where
a hunter proposed one, the dellingr-side workaround.

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

---

## BRK-002 - Workload pins cover comments, so fixing a stale header cuts history

Reported by: check-machinery, corpus-harness.

The pins are xxh128 digests of the file bytes (`corpus::bench_scripts` now
checks them in the gate). Comments are part of the bytes:
`examples/globals/write.lua`'s header is stale (see verif-corpus.md), and
correcting it re-pins the workload and starts a new results series, which is
exactly why such comments stay wrong.

Fix, brokkr side: pin a semantic fingerprint instead (the `analyze_cost` tree
`bench_scripts.rs` already computes, or the bytecode listing) so comment edits
do not cut history. That is a brokkr design call.

Enforceable: the pin's definition is brokkr's.
