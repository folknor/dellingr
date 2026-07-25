# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Target #62: public push methods do not enforce `MAX_STACK_SIZE`

The last non-deferred finding.

`MAX_STACK_SIZE = 1_000_000` is described as a global limit, but the public
push methods append directly with no `check_stack_space`
(`src/vm/stack.rs:87-118`):

| method | fallible today | checks the cap |
|---|---|---|
| `push_nil` | no | no |
| `push_number` | no | no |
| `push_boolean` | no | no |
| `push_rust_fn` | no | no |
| `push_string` | yes (string size) | no |
| `push_bytes` | yes (string size) | no |
| `push_value` | yes (index) | no |
| `push_named_rust_fn` | yes (registration) | no |

So a host `RustFunc` pushing in a loop never meets the cap. `check_stack_space`
is applied mainly when preparing Lua frames (`src/vm/eval.rs:363`).

Loop 17 closed the bulk-allocation half of this in `set_top`, which could
allocate a million values in a single call. This is the slower drip through the
same gap.

### The scope question this loop has to answer first

The finding itself notes this is "a documentation-versus-code mismatch as much
as a defect": it needs host code rather than script code, and a host that wants
to exhaust memory has easier routes.

Two honest resolutions, and they are different work:

1. **Make the cap real.** The four infallible pushes become fallible. That is a
   public API break and roughly **114 internal call sites** in `src/` alone,
   plus tests and examples. `push_bytes`/`push_string` are already fallible
   after the string-cap work, so part of the cost is paid.
2. **Make the documentation true.** State precisely that `MAX_STACK_SIZE`
   bounds VM-driven growth - Lua frames, `set_top` - and that host pushes are
   the host's responsibility, the same way the crate already says a State
   unwound through by a panic must be discarded.

**Do not simply do (1) because it is more work, or (2) because it is less.**
Decide which is true of the design, then execute it fully.

### Constraints

- Determinism unaffected; charge nothing new (#16).
- If (1): a rejected push must leave the stack unchanged, and `push_nil` is on
  hot paths, so the check must be a predictable branch, not a recomputation.
- If (2): the README, the `MAX_STACK_SIZE` doc comment and `AGENTS.md` must all
  agree, and `#62` should be deleted as "working as designed", not left open.
- `unwrap_used` denied outside `#[cfg(test)]`; clippy denies warnings.

---

## Agreed implementation plan - NOT YET IMPLEMENTED

Planned and reviewed, deliberately not started: see "Why this was not built in
the same session" at the end.

### Decision: make the cap real (resolution 1)

Argued from the crate's own stance, not from effort:

- Lua and Rust frames explicitly **share one stack**, so a "maximum stack size"
  naturally applies to both (`vm.rs:256-259`).
- Host callbacks already have an error channel built for exactly this:
  `RustFunc = fn(&mut State) -> Result<u8>` (`lua_val.rs:8`).
- A failing callback already truncates its frame and restores `stack_bottom`,
  leaving the State usable (`eval.rs:68`).
- The host API deliberately turns misuse into errors *before* mutation -
  `set_top`/`pop` (`stack.rs:25`), `set_table_raw` (`table_ops.rs:136`),
  `set_metatable_of` (`228`) - all done in this same series.
- Avoiding a host-process abort is already the stated reason for resource
  limits (`README.md:50`), and `MAX_STRING_BYTES` already applies through the
  host push API despite not being a complete memory sandbox.

"A malicious host can exhaust memory anyway" is not the relevant contract - the
same argument would delete `MAX_STRING_BYTES`. The cap protects the VM's own
invariant and catches accidental host misuse.

### Scope correction: the four methods are not enough

Changing only `push_nil`/`push_number`/`push_boolean`/`push_rust_fn` would
**not** make the cap global. `new_table` (`table_ops.rs:14`), `get_global`
(`vm.rs:612`), anchors (`anchor.rs:197`) and many VM paths grow the same
vector. Each needs either enforcement or an explicit proof that its growth was
already preflighted.

### Blast radius, assessed

90 real call sites in `src/` for the four methods (not the 114 a naive grep
suggests):

| area | calls | work |
|---|---:|---|
| standard library | 74 | all already in `Result`-returning bodies; add `?` |
| production VM core | 14 | 13 already `Result`; one needs a signature change |
| `#[cfg(test)]` | 2 | `expect`/`?` |

Plus ~80 signature-migration sites in `tests/`. The genuinely hard cases:

- `balance_stack` is infallible (`stack.rs:264`) and must return `Result<()>`.
- Rust-callback result padding cannot just use `?` - failure must still restore
  `stack_bottom` and truncate the frame (`eval.rs:73`, `102`).
- Generic-for callback normalization has the same cleanup problem
  (`eval_control.rs:244`).
- Lua frame setup already preflights the whole parameter/local allocation
  (`eval.rs:374`); keep the batch preflight rather than adding unreachable
  mid-prologue errors.
- `Frame::eval` contains unchecked direct pushes as well as public-method calls
  (`frame.rs:190`, `260`), so a true cap needs that wider audit.

### Granularity

Check **every logical operation that can increase `stack.len()`**. A single
push becomes unbounded by repetition, so there is no meaningful
bounded/unbounded split; and validating on callback return is too late, because
a callback can allocate indefinitely without returning.

- single public push: one predictable `len >= MAX_STACK_SIZE` branch
- known bulk growth (`set_top`, frame locals, varargs, result padding):
  preflight once, then an internal unchecked append
- net-neutral operations that pop before pushing: no extra branch
- **no cost charge added**

### Steps

1. Establish the invariant with three helpers: an inline single-slot check, an
   aggregate `check_stack_space(n)`, and a narrowly scoped unchecked internal
   append usable only after preflight or for net-neutral replacement.
2. All eight public push methods enforce the cap; the four infallible ones
   return `Result<()>`. Preflight strings before allocation and named functions
   before registration, so a rejected push leaves no side effect.
3. Migrate the 74 stdlib calls with `?`, then core and tests. `balance_stack`
   becomes `Result<()>`.
4. Audit every other growth path: make `new_table`, `get_global` and public
   `open_libs` fallible; update `push_anchor`, `call_anchor`, `table_next`,
   `get_metatable_of`, `table_remove_at`; preflight multi-value inserts and
   result padding; classify every raw `stack.push`/`insert`/`extend` as
   checked, preflighted, or net-neutral.
5. Preserve cleanup on failure in `State::call`, generic-for callbacks,
   metamethod dispatch and frame setup - a cap error must leave
   `stack_bottom`, call metadata and marker stacks clean.
6. Boundary tests: success at cap-1 and rejection at cap, every push type,
   unchanged stack on rejection, callback overflow cleanup then State reuse,
   named-function registration side effects, unchanged cost accounting,
   aggregate frame/result padding.
7. Update README, the `MAX_STACK_SIZE` comment and AGENTS.md to say the cap
   covers the shared Lua/Rust value stack but is **not** a total host-memory
   quota. Delete #62 once tests pass.

### Interaction with #59

Push-time enforcement does **not** replace the deferred saved-bytecode
verifier. #59 is about underflow, invalid local ranges, marker stacks and CFG
joins, which can panic without any growth. If every growth path is checked or
preflighted, that verifier no longer needs to prove
`height <= MAX_STACK_SIZE` for memory safety - it still must prove no
underflow, valid ranges, balanced markers and join agreement. Host callback
pushes are outside #59 entirely.

### Nothing existing should newly fail

No current behaviour approaches a million values. The largest deliberate
callback pushes 256 values (`tests/rustfn_error.rs:111`) and the GC test holds
20 strings with 1000 push/pop iterations (`src/vm/tests.rs:497`); both stay
valid and only need `?`. Many tests stop *compiling* on the signature change,
but none should return `StackOverflow`.

### Why this was not built in the same session

The finding reads as a small omission; the review established it is a
cross-cutting invariant touching most stdlib signatures, several public APIs
and every raw stack-growth site. Starting it late in a long session risked
landing it half-applied, which for a stack invariant is worse than not starting
- a partial audit gives the appearance of a guarantee without the guarantee.
The plan above is complete enough to execute directly.

### Superseded open questions

1. **Which resolution?** Argue it from the crate's own stance. Note the VM
   already treats host misuse as recoverable elsewhere - `set_top`, `pop`,
   `set_table_raw` and `set_metatable_of` were all made fallible rather than
   panicking, on the reasoning that a host mistake must not poison the process.
   Does that reasoning extend to memory exhaustion, or is exhaustion
   categorically the host's problem?
2. **If (1), what is the real blast radius?** 114 `src/` call sites is a count,
   not an assessment: how many are in `RustFunc`s that already return `Result`
   and so need only `?`, and how many are in infallible internal contexts that
   would need restructuring? Name the hard ones.
3. **If (1), does the cap belong on every push or only on unbounded ones?**
   A single `push_nil` cannot exhaust memory; a loop can. Is a per-push check
   the right granularity, or should the VM validate depth when control returns
   from a callback - bounding the damage without changing the API?
4. **Interaction with #59.** That deferred finding wants a stack-discipline
   verifier for saved bytecode. Does enforcing the cap at push time change what
   that verifier would need to prove?
5. Is there any existing test or example that pushes more than a trivial number
   of values and would newly fail?

Read `src/vm/stack.rs` (the push methods, `check_stack_space`), `src/vm.rs`
(`MAX_STACK_SIZE` and its doc), `src/vm/eval.rs:363`, `README.md`, and
`AGENTS.md`.
