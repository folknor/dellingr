# Hygiene hunt: VM execution core

Scope: `src/vm.rs`, `src/vm/{eval,eval_control,eval_index,eval_store,frame,stack,metamethod,lua_val,anchor,tests}.rs`.
Things I followed outside the scope are marked (outside).

Existing enforcement I read first: `[lints]` in `Cargo.toml`, the `clippy.toml`
disallowed methods/types (HashMap, `Result::ok`, unseeded rand, env/clock),
`unwrap_used`/`float_cmp` denied in `src/lib.rs`, the gremlins scan, and
`limits.rs` as the single home for limits. Overall the core is in good shape on
determinism and limits. What is left is mostly rules written out again at
several sites, plus encodings that are decoded by hand.

---

## 1. One value, one owner

### 1.1 SET_LIST batch stride: the VM spells it `u8::MAX`, the compiler spells it `MAX_TABLE_BATCH_SIZE`
`eval_store.rs` `instr_set_list` works out the start index as
`usize::from(batch) * usize::from(u8::MAX) + 1`. The compiler
(`compiler/parser/table.rs`) flushes batches at `limits::MAX_TABLE_BATCH_SIZE`.
Today both are 255, so they have not diverged. But if someone changes
`MAX_TABLE_BATCH_SIZE`, every array constructor longer than one batch silently
writes its elements to the wrong indices. That changes table contents, which is
replay-visible. Fix: have the VM read `MAX_TABLE_BATCH_SIZE`. Better still, give
`Instr` a `set_list_start_index(batch)` that both the encoder and the decoder
use. Enforcement: a parser test already pins the batch size. Add a VM-side test
of a 2-batch constructor that reads back `t[256]`. Or make the decode a method
on the instruction so a second spelling has nowhere to live.

### 1.2 Cache-slot "uncached" sentinels: two conventions, each decoded by hand at the site
- `GET_GLOBAL`, `GET_FIELD` and `SET_FIELD` use `255 = uncached`. The VM relies
  on `caches.x.get(255)` returning `None` because the vector is shorter.
  `frame.rs` passes a literal `u8::MAX` for `SET_FIELD_AT` to mean "no cache".
- `SET_GLOBAL` and `TFOR_CALL` use a biased encoding: `0 = uncached`, `n` means
  slot `n-1`. They decode it with `cache_idx.checked_sub(1)` in
  `eval_store.rs::instr_set_global` and `eval_control.rs::instr_tfor_call_next`.
- `instr.rs` documents the split, and `compiler/verify.rs` restates each rule
  with its own `u8::MAX` comparisons.

That makes four owners of "how does an operand name a cache slot": encoder,
verifier, VM decode, and the docs. The `255` convention is also only correct by
accident: it works because no cache vector ever reaches 256 entries. Nothing
asserts that. Fix: pick one encoding, and put a typed decoder on `Instr`
(`fn cache_slot(self) -> Option<u8>`) that the VM and the verifier both call.
Enforcement: a typed accessor makes a raw-operand decode stand out in review. A
verifier test can pin that cache vectors stay below the sentinel.

### 1.3 `OP_VARARG` all-values sentinel is a raw `u8::MAX`
`frame.rs` checks `if n == u8::MAX` for "push all varargs". `ArgCount`/`RetCount`
already own the 255 sentinel through `from_u8`. VARARG reimplements it, and so
does the verifier (`verify.rs` `OP_VARARG if a == u8::MAX`). Fix: decode the
operand with `RetCount::from_u8`. Enforcement: a text rule (rg for `== u8::MAX`
in `src/vm/`) or a typed accessor.

### 1.4 Global read/write rule is restated at four sites
The rule has three parts:
- builtin names live in `builtins[]` and are mirrored into `globals`;
- writing nil to a never-defined name is a no-op, while an existing name keeps
  a tombstone;
- a builtin rebind bumps `globals_version`.

It is implemented independently in:
- `vm.rs::set_global_value_owned` (the documented central setter);
- `eval_index.rs::instr_set_builtin`. It restates the tombstone rule by hand
  and bumps the version unconditionally, which agrees with the central setter
  only because every caller is a builtin;
- `eval_store.rs::instr_set_global`. Its slow path writes `globals` in place for
  existing non-builtin names, bypassing the setter, with a comment explaining
  why that is safe;
- the cached SET_GLOBAL fast path (`try_set_global_cached`), which writes nil in
  place. That is the reason the tombstone rule exists at all.

The read side is duplicated too: "`Builtin::from_name`, then `globals`" appears
in `vm.rs::get_global`, `eval_index.rs::instr_get_global` and
`with_restricted_env`. It probably also appears in the `_G` proxy in
`lua_std/basic.rs` (outside, not verified).

None of these copies disagree today. But the invariant "builtins[i] and
globals[name] agree" has at least four writers who never call each other. A
fifth writer that forgets the mirror makes `_G.print` and `print` disagree.
Fix: one `GlobalEnv` type that owns `globals`, `builtins` and `globals_version`,
with `get`, `set`, `set_cached_index` and `swap_for_restricted`. Make the fields
private so the VM cannot reach around it. Enforcement: structural, via module
privacy. Add a debug assertion on GC mark that every builtin slot equals its
`globals` entry where one exists.

### 1.5 Metamethod names are interned by string literal at every dispatch site
There are six `alloc_string("__index")`-style interns in scope: `metamethod.rs`
(×2), `eval_index.rs` (×2), `eval_store.rs` (`__len`) and `eval.rs`
(`__call`), plus `table_ops.rs` `__tostring` (outside). Each site does its own
GC-rooting dance (push receiver/key, intern, pop). They disagree on error
policy:
- `metamethod.rs` and `index_string` use `?`;
- `eval.rs`, `instr_length` and `protected_index_key` use
  `.expect("fixed metamethod name is far below MAX_STRING_BYTES")`.

`table_ops.rs` takes a third approach: a non-allocating `find_string` probe
first. There are three policies for the same operation. Every metatable miss on
a read also allocates, and may therefore collect, which is why the rooting dance
is needed at all. The comments say each site's current GC timing "is part of the
collection behavior". So the timing is now load-bearing at six places.

Fix: an `enum MetaName { Index, NewIndex, Len, Call, ToString, ... }`. Intern
the names once per State, as GC roots in `mark_gc_roots`, the way `Builtin`
slots are. Read them as `self.meta_names[MetaName::Index]`. That removes every
dispatch-time allocation, every rooting dance, and every expect/? disagreement.
This changes GC timing, which affects `%p` ids and collection points. Those are
deterministic but snapshot-visible, so call the change out. Enforcement: an rg
text rule banning `alloc_string("__` outside the interning table, or a
`disallowed`-style check in the gremlins scan.

### 1.6 Stdlib capacity hints are hand-counted against another module
`vm.rs` has `STDLIB_INTERNED_TABLE_KEYS = 41`, `STDLIB_HEAP_TABLES = 6` and
`STDLIB_NON_BUILTIN_GLOBALS = 1`, each with "keep in step with the registrations
in `lua_std`". Nothing checks them. Since they are hints only, drift costs a
reallocation, not correctness. Fix: a unit test that opens the libs and asserts
`heap.string_count()`/`object_count()` against the constants. Or drop the
constants and reserve from counts that `lua_std` exports. The other scattered
capacity hints have no owner and no rationale beyond the one comment:
`stack: Vec::with_capacity(256)`, `call_stack: 64` and `transient_roots: 64` in
`empty_with_callbacks`. Group them next to the stdlib hints.

### 1.7 Opcode cost is restated in dispatch comments
`frame.rs` labels arms with "costs 1", "Table reads are free", "comparisons are
free" and so on. The owner of that fact is `Instr::static_units()`, and the arms
correctly read it through `const { ... }`. The comments are a second copy.
`docs/budget.md` is presumably a third. If someone changes a charge in
`static_units`, the dispatch comments and the docs lie. Fix: delete the numeric
claims from the comments. Generate or test the budget.md table from
`static_units` (a doc test that renders the table and compares it).
Enforcement: that test.

### 1.8 `COST_CHECK_INTERVAL` is re-aliased
`Frame::COST_CHECK_INTERVAL = crate::limits::COST_CHECK_INTERVAL`, and it has a
paragraph-long docstring of its own. That is harmless, but it is a second name
for one knob. Read the limits constant directly.

### 1.9 `STRING_METATABLE_TOKEN` vs `"_G.metatable"`
`capture_env_tokens` names one snapshot token through a constant and the other
through an inline literal. The builtin-slot tokens come from `slot.name()`. The
loader in `save_state` (outside) must spell the same strings. Put all token
names in one table owned by save_state.

---

## 2. Values nobody can find, change, or trust

- **Tunables are well-centralised.** `limits.rs` answers "what are the limits",
  and its header explains why there is no injection point (replay/snapshot
  contract). That is a real answer, not a finding. The exceptions are the items
  in 1.6 (capacity hints living in `vm.rs`) and the `Frame` alias.
- **Cost meter state has no single owner type.** `cost_remaining`,
  `cost_budget`, `cost_budget_configured` and `cost_used` are four loose `State`
  fields. `CostMeter::runtime(...)` is built from three of them in two places:
  `cost_meter()`, and the hand-destructuring in
  `gmatch_subject_matcher_and_cost_meter`. `cost_batch_needs_flush` reads them
  directly. A fifth reader can easily get the "unconfigured budget" rule wrong.
  Fix: one `CostState` struct field that owns all four and hands out
  `CostMeter`. The borrow split in gmatch then becomes "borrow `self.cost`",
  not four fields.
- **`gc_threshold` initial value** lives in `GcHeap::new()` (outside). There is
  no way to construct a State with a chosen initial threshold except mutating it
  after construction. By then `open_libs` has already run under the default.
  That is fine for replays, but tests that want GC during stdlib setup cannot
  get it.

---

## 3. One channel, one implementation

- **`debug_vm` prints straight to stdout** (`frame.rs`: `println!("{inst:?}")`).
  So do `debug_gc` (`object.rs`, outside) and `debug_parser` (`parser.rs`,
  outside). The project has a channel, `HostCallbacks`. These bypass it and
  interleave with `DefaultCallbacks::on_print`, which also writes to stdout
  (`host.rs`). Under `debug_vm` you therefore cannot separate script output from
  the trace, and an embedder's capture sees neither. The trace line also omits
  ip, frame and source, so it cannot be acted on in a multi-frame run. Fix:
  route traces to stderr through one `trace!` macro with a single
  implementation, or a `HostCallbacks::on_trace` hook. Include `ip` and
  `bytecode.name`. Enforcement: clippy `print_stdout`/`print_stderr` set to
  deny in the library (allowed in `main.rs`, `examples/hotpath.rs` and the one
  trace macro). This build can enforce that today.
- **`InternalError(String)` is assembled ad hoc at about 20 sites in scope.**
  Each site writes a free-text message ("SetList(0): no constructor base",
  "call-base marker is below the active frame", "unknown opcode: N",
  "InitField: expected table, got {typ}", ...). None carries the opcode, ip or
  chunk as structured data. The location is filled in later by
  `locate_in_frame`, but only as a line number. Many of these are "verifier
  regression" reports, so the useful identifiers are the instruction index and
  the opcode. Fix: `ErrorKind::VmInvariant { opcode: u8, ip: usize, what:
  &'static str }`, or a small enum of invariant kinds, filled in at the frame
  boundary. Enforcement: by type. `InternalError(String)` disappears from the VM
  and `rg` confirms it.
- **Opcode-level errors take a source line from `frame.current_line()` only.**
  An `InternalError` raised while `call_stack` has not been updated for the
  current opcode is fine for the innermost frame. Outer frames depend on
  `record_call_site` having been called. See 5.3 for the per-opcode placement
  of that call.

---

## 4. Errors

### 4.1 Public stack API swallows invalid indices
In `stack.rs`:
- `to_boolean(idx)` returns `false` for an invalid index;
- `typ(idx)` returns `LuaType::Nil`;
- `raw_equal(a, b)` returns `false`.

An embedder's off-by-one is then indistinguishable from a real nil/false. Every
other host API (`to_number`, `to_bytes`, `copy_val`, `insert`) returns
`InvalidStackIndex`. Fix: make them `Result` (pre-1.0 permits it), or add
`try_` forms and deprecate these. This mirrors the C API's "acceptable index",
which is a deliberate convention there. Here it is inconsistent with the rest of
the same API.

### 4.2 Panics on bytecode-controlled conditions
These panic rather than refuse. Each is safe only because the verifier ran, and
nothing in the type system says the bytecode was verified:
- `OP_SWAP`: `stack.swap(len - 1, len - 2)` underflows and panics on a short
  stack;
- `OP_DUP`: `.expect`;
- `pop_val`: `expect("VM bug")`, reached directly by `OP_POP`, `OP_EQUAL` and
  every binary op;
- `instr_set_local`/`instr_get_local`/`instr_tfor_*`: unchecked
  `stack[base + n]`;
- `frame.upvalues[idx]`, `bytecode.nested[i]`, `number_literals[i]` and
  `string_literals[string_num]`: unchecked indexing.

Snapshots load bytecode from storage. If save_state's load path ever skips
verification for one chunk (outside, not verified), any of these turns a corrupt
save into a host panic, where `LoadError` was owed. `Frame::jump` and
`instr_set_list_count` show the opposite policy: they keep a defensive check
"so a verifier regression returns a VM error". Two policies for the same class
of input. Fix: a `VerifiedBytecode` newtype that only the verifier can
construct, with `Frame` accepting only that type. Then either drop the
defensive checks everywhere (they are unreachable by construction) or keep them
everywhere. Enforcement: type/signature.

### 4.3 `Val::hash` panics on NaN; `to_bytes_with_heap` is `unreachable!` for functions/objects
Both are documented crate invariants, and both make the function partial in a
way the type does not show. `to_bytes_with_heap` in particular is a
`pub(super)` function with a panic for 2 of its 6 variants. Fix: take a type
that excludes the cases (a `Primitive` view), or return `Option`.

### 4.4 `State::error(&self, kind)` ignores `self`
The comment calls it a "public compatibility helper". It is
`Error::without_location(kind)`. VM code uses both spellings
(`self.error(...)` in most places, `Error::without_location(...)` in
`eval_closure`, `frame.rs`, `stack.rs` and `anchor.rs`), which suggests one of
them does something the other does not. Pick one inside the crate.
`without_location` is the honest one. Keep `State::error` only if hosts need it.

### 4.5 Non-function `__call` error reporting is a third reporting boundary
`call_op` reports to `on_error` through `report_call_error`, through the inline
`host_error` in the `__call` chain block, and through a third inline copy for
"not callable". All three restate the predicate
`call_depth == 0 && call_chain_depth == 0 && active_rust_fn.is_none()`.
`report_call_error` exists, and the two inline copies should call it. The first
one cannot, because it needs `handler_is_function`, but it can call a shared
`is_outermost_host_boundary()` predicate.

---

## 5. Policy invented per call site

### 5.1 The Rust-function call ABI is implemented twice
`eval.rs::call_op` (RustFn branch) and `eval_control.rs::instr_tfor_call_rust_fn`
each implement the same steps independently:
- set `stack_bottom`;
- save/restore `active_rust_fn`;
- call `f`;
- on error, truncate and restore;
- normalise the reported return count (pad with nil when reported > actual,
  `rotate_right` when less);
- restore `stack_bottom`.

They have already diverged in one place. `call_op` routes errors through
`report_call_error`; the tfor copy returns them raw and relies on the enclosing
frame to report. That is correct today because a frame always encloses a TFOR,
but it is a second definition of "where a native error is reported". Fix: one
`invoke_rust_fn(f, call_base, nargs) -> Result<u8>` used by both. The tfor
fast path then only skips pushing the callee. Enforcement: structural. After the
extraction, rg for `active_rust_fn.replace` should find one site.

### 5.2 SetList(0) base validation is duplicated verbatim
`instr_set_list_count` and `instr_set_list` contain the same three checks with
identical strings: no base, base out of frame, base not a table. One `peeks`
and one `pops`. Fix: `fn constructor_base(&self) -> Result<usize>`, called by
both.

### 5.3 Call-site recording and cost flushing are placed by hand per opcode
`record_call_site(state)` is called before `OP_CALL`, `TFOR_CALL`, `LENGTH`,
`GET_FIELD`, `GET_TABLE`, `SET_FIELD`, `SET_FIELD_AT` and `SET_TABLE`: any
opcode that might re-enter Lua through a metamethod. `flush_local_cost` is
likewise called by hand in each metamethod helper before it calls out
(`metamethod.rs` ×4, `eval_index.rs`, `eval_store.rs`). Nothing ties "this
opcode can call out" to "this opcode records its site and flushes". A new opcode
that reaches a metamethod and forgets either one produces a wrong traceback
line, or a `cost_used` that depends on the batch interval, which is
replay-visible. Fix options:
1. Make `State::call` itself flush, taking `local_cost`.
2. Record the ip into `CallInfo` unconditionally on every call out of the
   dispatch loop, which is cheap: it is one store.
3. Add an `Instr::may_reenter()` table that the dispatch consults and a test
   cross-checks.

Enforcement: a test that runs each opcode against a metatable'd operand and
asserts traceback line and `cost_used`.

### 5.4 Cleanup-on-error is hand-written per path
`call_op` has 6 `truncate_call_frame` calls. `eval_closure_frame` has 3
`cleanup_closure_frame` calls. `handle_index_metamethod_inner` and
`index_string` each truncate a temporary on error. Watermark truncation of the
vararg/table-constructor base stacks lives in `eval_closure`. That is the right
shape, but the policy is written out per exit. A drop guard (or a
`frame_scope(|s| ...) -> Result` helper) would make "every exit restores" true
by construction. The concern is stack-frame bloat on the recursive path, as
`eval.rs` explains. A non-closure RAII struct holding `&mut State` does not add
a landing pad when it has no `Drop` side effects beyond plain stores. Measure
before rejecting it.

### 5.5 Panic-safety is decided per site, and the sites contradict each other
`eval.rs` states the policy: "a State unwound through must be discarded, not
reused", and the main Lua call path deliberately has no guard. Yet three sites
use `catch_unwind` + `resume_unwind` to restore their own bookkeeping:
- `with_restricted_env`, whose comment says it is "so an embedder that catches
  the panic and reuses the State" works. That directly contradicts the policy;
- `with_rooted_values`;
- the vararg path of `eval_closure_inner`.

A host reading `with_restricted_env` would conclude that reuse after a panic is
supported. Fix: decide. Either (a) a poison flag set on unwind and checked at
the public entry points (`call`, `load_string`, push APIs) so reuse is refused
loudly, or (b) remove the three guards. (a) costs one branch per host call, not
per opcode, because the check belongs at the public boundary, not in
`eval_closure`. The `eval.rs` comment rejects a poison flag because it would sit
"on the hot entry path"; that applies only if it is checked per Lua call.
Enforcement: a test that panics inside a RustFn under `catch_unwind` and asserts
that the next `call` refuses.

---

## 6. Ambient dependencies

- **`anchor::NEXT_STATE_ID` is a process-global `AtomicU64`.** It is not
  replay-visible (Anchor's Debug is documented as non-deterministic). But it is
  the only process-global mutable state in the VM, and it makes `State` creation
  order across threads observable through `Anchor`'s `Debug`/`Hash`. If an
  embedder ever hashes Anchors into a deterministic structure, order depends on
  thread scheduling. Alternative: a per-State random-free identity such as the
  State's heap address at construction plus a generation. Or accept it and keep
  `Hash` off `Anchor`: removing `Hash` closes the footgun.
- **`Val::Hash` for `RustFn` hashes the function address** (ASLR-dependent).
  Tables are IndexMap with insertion order, so `pairs` order is unaffected. The
  bucket layout differs per process, which matters only for performance. Worth
  one comment beside the impl so nobody later "fixes" table order by switching
  to a hash-ordered map.
- No clock, environment, cwd or randomness reached from scope. The clippy bans
  hold.

---

## 7. Shared state whose safety is not structural

- **`set_table_with_key` may run user code while `instr_set_field` holds `idx`.**
  `instr_set_field` keeps a stack index across a `__newindex` call and checks it
  only with `debug_assert_eq!(self.stack[idx], tbl_val)`. The comment admits
  this is "no current path" safety. In release builds a violation silently
  removes the wrong slot (`self.stack.remove(idx)`). Same pattern:
  `handle_index_metamethod_inner` reads `self.stack[table_idx]` after a flush,
  and `instr_tfor_call` writes `stack[base + 3 + i]` after calling arbitrary
  code. Safety rests on the invariant that called frames live strictly above
  `stack_bottom` and never write below it. That holds structurally for Lua
  frames. For RustFns it holds only because the public API's `convert_idx`
  refuses indices below `stack_bottom`. Stating that as the invariant, with a
  test, would make it structural.
- **`vararg_call_bases` / `table_constructor_bases`** are shared across frames,
  with balance kept by the bytecode and repaired by watermarks on error. The
  defensive checks ("base below current frame") exist because a RustFn running
  during argument evaluation sees the caller's pending base. Per-frame storage
  (the bases inside `Frame`) would make cross-frame confusion unrepresentable,
  and would delete the watermark logic and three `InternalError` checks.
- **`globals_version` has three bump sites**: `set_global_value_owned`,
  `instr_set_builtin`, and `with_restricted_env` (twice). Covered by 1.4.
- **`string_mt_index_shape` is reset only in `install_string_metatable`.** A
  stale entry "only causes misses" (comment). That holds as long as
  `get_cached_string_method` re-checks the slot value, which it does. This
  invariant is structural.

---

## 8. Unbounded growth

- **`bytecode_caches`**: every `load_string` creates a fresh `Arc<Bytecode>` and
  an entry. These are swept only by GC, so with `gc_disable_auto()` a host that
  loads code repeatedly grows without bound. The same holds for
  `format_pointer_ids` (documented). Both are reasonable, but neither is listed
  in `docs/limits.md`'s "what can grow" (not verified).
- **`open_upvalues`** uses `Vec::insert` in sorted order. Its size is bounded by
  live stack slots (≤ `MAX_STACK_SIZE`), but a single insert is O(n), so a
  script capturing many locals in a deep frame pays quadratic time, uncharged.
  Per the budget doc that is a performance concern, not a cost defect.
- **Rust-function re-entrancy is not counted against `call_depth`.** A RustFn
  that calls `state.call` on another RustFn recurses on the native stack with no
  VM limit. Only Lua frames increment `call_depth`. That is host-controlled, so
  it is low risk, but `docs/limits.md` describes `MAX_CALL_DEPTH` as bounding
  "simultaneously active Lua calls". That is accurate, but easy to misread as
  bounding all calls.
- **`globals` tombstones** are bounded by distinct names (documented).
  `rust_fns_by_id` grows by design (documented).

---

## 9. Code that is no longer load-bearing

- **`State::get_string_constant(&self, frame, i)`** is a one-line forward to
  `frame.literal(i)`, and it carries `#[hotpath::measure]`. The measurement on
  an accessor this small mostly measures itself. Nothing else is done there.
  Evidence: the body is the whole function. Inline it at its 4 call sites.
- **`State::error`** (4.4): its own comment calls it a compatibility shim.
  Within the crate it is equivalent to `Error::without_location`.
- **`Frame::new`'s `let ip = 0;`** and `Frame::bytecode()` vs direct field
  access: trivial.
- **`Frame::COST_CHECK_INTERVAL`** alias (1.8).
- **The historical-bug narration in `call_op`** ("IMPORTANT: ... Previously,
  using `f(self)?` here caused a bug ... segfaults"). It describes code that no
  longer exists and claims release-build segfaults the current safe code cannot
  produce. Replace it with a one-line statement of the invariant.
- **`InternalError("fused branch dispatched with a non-fused opcode")`** and
  `"unknown opcode"` are unreachable when the dispatch range arm and the
  verifier are correct. Under 4.2's `VerifiedBytecode` they become
  `unreachable!` or disappear.
- **`debug_vm` feature**: a single `println!`. Either make it useful (3) or
  delete it. Evidence: nothing in brokkr.toml, scripts or tests enables it.
- I could not find dead modules in scope. `tests.rs` is live.

---

## 10. Host assumptions and writes

- **No filesystem, environment, locale or core-count access in scope.**
- **NaN sign / `%.14g`**: `number_to_string` explicitly neutralises the
  architecture-dependent NaN sign and relies on Rust's correctly rounded
  formatting. That is detected and handled, not assumed.
- **Native stack size is assumed sufficient** for `MAX_CALL_DEPTH` Lua frames
  plus `MAX_METAMETHOD_DEPTH` metamethod frames. On a host thread with a small
  stack (embedders running scripts on a worker thread with a default 2 MiB
  stack), deep recursion aborts the process instead of returning
  `CallDepthExceeded`. That is a silent failure mode, documented in
  `docs/limits.md`. It is not detected: there is no stack-remaining probe (for
  example the `stacker`/`psm` approach) and no per-State depth override. The
  `eval.rs` comments show the team already treats per-frame stack bytes as
  load-bearing (the no-`catch_unwind`/no-hotpath rules on the recursive path),
  so the stack-size assumption is an implicit config value. The
  `call_depth_exceeded_error` test is the only enforcement, and only for the
  test thread's stack size.
- **`Vec::with_capacity(256)` and friends** assume a "typical" script. They are
  hints only.
- **Test `vm/tests.rs`**: several tests push `MAX_STACK_SIZE` (1M) values,
  which assumes about 16 MB of free memory per test thread. Fine on dev hosts.
  Worth knowing under `test_limited.sh`'s 2 GiB cap with many parallel test
  threads.

---

## Lateral findings

- **`call_op` pads Rust results when `reported > actual`** with nils, where
  reference Lua would read below the callee's frame or raise an error. That is a
  quiet leniency: a RustFn returning `Ok(3)` after pushing 1 value yields
  `v, nil, nil`. It is not listed in `docs/compatibility.md` (as far as I
  checked); it should either be listed or become an error (it is host misuse).
- **`concat` (public) checks `n > self.stack.len() - self.stack_bottom`**, which
  is `get_top()` spelled differently from every other count-taking API.
- **`instr_set_builtin` writes `globals.insert(builtin.name().to_string(), val)`**:
  it allocates a String on every builtin assignment to an existing name.
  Use `get_mut` first. This is a small hot-path allocation.
- **`instr_get_global` for a builtin name** never populates its cache. That is
  deliberate (builtins compile to GET_BUILTIN), but the sandbox case
  (`with_restricted_env`) depends on that compile-time split staying exact.
- **`get_index_table_field_direct`** builds the same `MethodLookupCacheEntry`
  four times, differing in two fields. A constructor closure would remove 30
  lines and the chance of one copy missing `globals_version`.
