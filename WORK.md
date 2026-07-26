# WORK.md

Current work item. Optimization loop 2 of the measured sequence.

---

## Target: stop allocating on every Lua call - `Closure.upvalues` as `Arc<[UpvalueRef]>`

### The mechanism

`State::call` -> `Val::as_lua_function` -> `GcHeap::as_lua_function`
(`object.rs:234-239`) clones the `Closure` on every Lua-to-Lua call. The
`bytecode` and `caches` fields are `Arc`s (refcount bumps), but
`upvalues: Vec<UpvalueRef>` deep-copies - a heap allocation per call for any
closure with captured upvalues. The clone exists because `initialize_frame`
moves the upvalues into the `Frame` (`eval.rs:546`, `frame.rs:36`).

### Measured evidence (2026-07-26, plantasjen)

`--alloc` on `numerics/arithmetic` (uuid 93903c8e, commit 4827ac5):
`as_lua_function` allocates 4.3 MB across 1,116,493 calls - 20% of the
workload's entire process allocation, on a bench whose Lua heap is static
(the `fib` closure captures one upvalue, so every recursive call pays the
Vec clone). `--hotpath` shows `as_lua_function` at 1.3-3.8% of wall across
call-heavy workloads.

### The change

`Closure.upvalues` and `Frame.upvalues` become `Arc<[UpvalueRef]>`; the
clone becomes a refcount bump, the frame handoff a move of the same Arc.
Conversion from the construction-time `Vec` happens once, inside
`GcHeap::alloc_lua_fn` (which keeps its `Vec<UpvalueRef>` parameter so no
caller changes shape). A per-heap cached empty `Arc<[UpvalueRef]>` serves
closures without captures, so their construction stays allocation-free
(a naive `Vec::new().into()` would ADD an Arc-header allocation where
today there is none).

Verified safe by reading every access site: construction happens once in
`instr_closure` (`eval_control.rs:37-52`); readers are indexed
(`frame.upvalues[i]` in the two upvalue opcode handlers in `eval_index.rs`
and the capture loop in `eval_control.rs`); GC marking iterates
(`object.rs:412`); the save walker iterates (`save_state.rs:714`) and the
load path builds a `Vec` and hands it to `alloc_lua_fn`
(`save_state.rs:1051-1055`). Nothing mutates the collection after
construction - upvalue writes go through the `UpvaluePool` slots the refs
point at, never through the Vec.

### Constraints (inline; sessions read nothing else)

- Read and write code only; no cargo/brokkr/test/bench commands.
- Determinism unaffected by design: identical allocation order, identical
  `cost_used` (this change touches no cost accounting).
- Clippy denies warnings; `unwrap_used` denied outside tests;
  `Result::ok()` banned; HashMap/HashSet banned.
- Snapshot format untouched: encode/decode already work over iteration and
  a construction-time Vec.
- `#[hotpath::measure]` on `as_lua_function` stays - its mean falling while
  call counts hold is the measurement of success.

### Verdict plan (orchestrator)

- `arithmetic` A/B interleaved worktree pairs vs commit 11854a8 - the
  fib-recursion clone is the target; expect a small single-digit% wall
  win at best (the allocation was 20% of alloc volume, not 20% of time).
  Honest expectation: may be within noise; the alloc-mode delta is the
  primary verdict.
- `--alloc` on arithmetic after: `as_lua_function` exclusive bytes should
  collapse from 4.3 MB to ~0.
- `closure` and `gc_churn` guard creation-heavy paths (each closure
  construction now converts Vec -> Arc; must not regress).
- Cost fingerprints identical everywhere.
