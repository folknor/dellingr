# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #7, #8, #9: parser and codegen hardening

Three independent defects that all live in `src/compiler/parser/`. Grouped so
one build session touches those files once. They do not share a mechanism.

All three verified: #9 empirically (wrong line reported), #7 and #8 by reading.

### #7 (High) - unbounded parser recursion aborts the host process

The parser is recursive descent with no depth bound anywhere.
`Parser::nest_level` (`parser.rs:30, 68, 89, 372, 382`) tracks *scope* depth
for local-slot bookkeeping and is never compared against a limit; there is no
`syntax_depth` equivalent.

Mutually recursive cycles that hostile source can drive arbitrarily deep:

- `parse_expr -> parse_or -> ... -> parse_primary -> parse_prefix_exp ->
  parse_expr` via parentheses (`expr.rs`)
- `parse_unary -> parse_unary`, `parse_pow -> parse_unary -> parse_pow`
- `parse_statements -> parse_do` (`stmt.rs:416-423`)
- `parse_if_arm -> parse_else_or_elseif` (`stmt.rs:503-547`)
- `parse_table -> parse_table_entry -> parse_expr -> parse_table` (`table.rs`)
- recursive prefix-extension chains (`a.b.c.d...`)

Reference Lua rejects this with "chunk has too many syntax levels"
(`LUAI_MAXCCALLS`, ~200). dellingr instead exhausts the native stack, and a
Rust stack overflow is an abort - it cannot be caught or returned as a
`SyntaxError`, so a hostile script kills the whole game process. This is the
parser-side sibling of the lexer comment-recursion fix (L17): the lexer was
hardened, the parser was not.

Hostile inputs: 200k nested `(`, 200k of `do `, 200k of `-` before a literal,
200k nested `{`.

**Do not run these in-process while testing without expecting an abort.**

### #8 (High) - upvalue index truncation past 255 silently miscompiles

`src/compiler/parser/upvalue.rs`. Four sites cast a list length to `u8` with no
cap:

```rust
fn add_upvalue(&mut self, name: &str, desc: UpvalueDesc) -> u8 {
    let idx = self.upvalues.len() as u8;   // line 102
    self.upvalues.push((name.to_string(), desc));
    idx
}
```

plus `create_parent_upvalue` at lines 69, 79 and 90
(`self.outer_upvalues[parent_idx].len() as u8`).

`add_local` (`parser.rs:85-95`) *is* capped at 255, and reference Lua caps at
`MAXUPVAL` 255 with "too many upvalues". A function can legally reference more
than 255 distinct outer names - 200 locals in a grandparent, 60 in the parent,
an inner function referencing all 260 - and the 256th upvalue silently gets
index 0. `Instr::get_upvalue(0)` then reads the wrong variable: no error, no
panic, a miscompiled program.

Worse, `UpvalueDesc::Upvalue(idx)` is stored into `Bytecode.upvalues` with the
truncated index, so the closure capture list itself is wrong and the snapshot
codec faithfully persists the wrong program.

### #9 (High) - `line_info` desyncs from `code`

`Parser::push` (`parser.rs:385-389`) appends to `code` and `line_info` in
lockstep, which is the only thing keeping them aligned:

```rust
fn push(&mut self, instr: Instr) {
    self.chunk.code.push(instr);
    self.chunk.line_info.push(self.current_line);
}
```

Ten sites then mutate `code` directly and never touch `line_info`:

- `expr.rs:272` and `expr.rs:327` - `self.chunk.code.remove(mark_idx)`, which
  runs for **every plain fixed-arg call**. This both shortens `code` and
  misaligns every entry at or after `mark_idx`.
- `expr.rs:257, 262, 312, 317` - pop-then-push adjustment for vararg and
  tail-call argument counts, each leaving one stale extra entry.
- `stmt.rs:24, 29` - return-tail adjustment.
- `stmt.rs:371, 383` - `adjust_multi_assign`.

Consumers index `line_info` by pc: `vm/frame.rs:70` (`current_line`, stack
traces), `vm.rs:488` (`host_print` line), `compiler.rs:263`
(`assign_cache_slots` error line). `save_state.rs` serializes the skewed
vector, so a snapshot persists the wrong line table.

Verified: a script whose only error is on line 5, preceded by 20 calls,
reports line 4 in dellingr and line 5 in reference Lua 5.4. Skew accumulates,
so call-heavy code drifts further.

The invariant is also directly testable without running anything:
`bc.code.len() == bc.line_info.len()` recursively after `finalize` fails today
for something as small as `print(1)`.

One correction to the above: the `host_print` line consumer is `vm.rs:503`
(and `vm.rs:744-749`), not `vm.rs:488`.

---

## Agreed implementation plan

Land in order **#9, then #8, then #7**. #9 adds the `finalize` assertion and
the invariant test that the other two then run under. All three are
parser-local and error-path additive: every currently-accepted program still
compiles to byte-identical bytecode, so the audited jump / break-jump /
table-template / tail-call bookkeeping is untouched.

### C. #9 - mirror `code` edits into `line_info`

Chosen over the OP_NOP restructure (`optimizations.md` #7). Mirroring provably
emits byte-identical bytecode because it only touches `line_info`; OP_NOP
changes the instruction stream and would ripple into VM dispatch,
`analyze_cost`, the save codec, and every recorded jump offset. OP_NOP stays a
perf item, and these helpers make that later migration mechanical.

Two helpers next to `Parser::push` in `src/compiler/parser.rs`:

```rust
/// Removes the instruction at `idx`, keeping line_info aligned.
fn remove_instr(&mut self, idx: usize) -> Instr {
    self.chunk.line_info.remove(idx);
    self.chunk.code.remove(idx)
}

/// Overwrites the last emitted instruction in place, returning the old one.
/// line_info keeps the original line, which is what we want when rewriting a
/// Call/Vararg ret-count in a tail position.
fn replace_last_instr(&mut self, instr: Instr) -> Instr {
    let slot = self
        .chunk
        .code
        .last_mut()
        .expect("replace_last_instr requires a previously emitted instruction");
    std::mem::replace(slot, instr)
}
```

Rewrite the ten structural sites:

- `expr.rs:272`, `expr.rs:327` -> `self.remove_instr(mark_idx)`.
- `expr.rs:257`, `expr.rs:312`, `stmt.rs:29` -> `replace_last_instr(Instr::vararg(u8::MAX))`,
  dropping the following `self.push`.
- `expr.rs:262`, `expr.rs:317` -> read first
  (`let old = *self.chunk.code.last().expect(...)`), keep the existing
  `unreachable!` check on `old.opcode() == Instr::OP_CALL`, then
  `replace_last_instr(Instr::call(ArgCount::Fixed(old.a()), RetCount::All))`.
- `stmt.rs:24` -> same read-then-replace with
  `Instr::call(ArgCount::Fixed(num_args), RetCount::All)`.
- `stmt.rs:371`, `stmt.rs:383` (`adjust_multi_assign`) ->
  `let old = self.replace_last_instr(...)`, keeping the existing
  `debug_assert!` on `old`.

Converting the pop+push pairs into in-place rewrites is a side benefit: the
rewritten instruction keeps its original line instead of being restamped with
`current_line`.

**Do not** touch the in-place rewrites that were already correct - they change
no vector length and cannot desync: `patch_jump` (`parser.rs:131`), the ForPrep
back-patch (`stmt.rs:266`), the table-constructor patches (`table.rs:110, 115,
118, 125, 200, 202`), and `assign_cache_slots` (`compiler.rs:220`).

Add as the first line of `finalize` (`src/compiler.rs:357`):

```rust
debug_assert_eq!(bc.code.len(), bc.line_info.len(), "line_info desynced from code");
```

`finalize` already recurses over `nested`, so this covers the whole tree.

### B. #8 - cap upvalues at 255

`src/error.rs`: add `SyntaxError::TooManyUpvalues`, Display `"too many upvalues"`.

`src/compiler/parser/upvalue.rs`:

- `add_upvalue` -> `Result<u8>`, erroring when
  `self.upvalues.len() >= u8::MAX as usize`, exactly mirroring `add_local`.
- `create_parent_upvalue` -> `Result<Option<u8>>`, checking the same bound
  before each of the three pushes (lines 69-71, 79-81, 90-92).
- `resolve_upvalue` / `resolve_upvalue_recursive` -> `Result<Option<u8>>`,
  propagating with `?`.
- Sole external caller `parse_prefix_identifier` (`parser.rs:582`) becomes
  `if let Some(i) = self.resolve_upvalue(name)? {`.

Four push sites is the complete set. The other `as u8` casts in the file
(`find_upvalue` line 11, `position(...)` lines 41 and 77, `Upvalue(...)` line
81) cast *positions within* those lists and become safe automatically once
length is capped; the local-index casts (lines 33, 71) are already covered by
`add_local`'s cap.

### A. #7 - syntax depth guard

`src/error.rs`: add `SyntaxError::TooManySyntaxLevels`, Display
`"chunk has too many syntax levels"` (reference Lua's wording). Leave
`is_recoverable` matching only `UnexpectedEof`.

`src/compiler/parser.rs`: `const MAX_SYNTAX_DEPTH: u32 = 200;`, a
`syntax_depth: u32` field on `Parser` (init 0), and:

```rust
fn enter_syntax_level(&mut self) -> Result<()> {
    if self.syntax_depth >= MAX_SYNTAX_DEPTH {
        return Err(self.error(SyntaxError::TooManySyntaxLevels));
    }
    self.syntax_depth += 1;
    Ok(())
}

fn exit_syntax_level(&mut self) {
    self.syntax_depth -= 1;
}
```

**One counter, but it must be incremented at all five of these choke points.**
Any subset misses a cycle:

1. `parse_expr` (`expr.rs:34`) - parens, table nesting, call-arg nesting.
2. `parse_unary` (`expr.rs:158`) - `- - - -x`, `not not ...`, and
   `parse_pow -> parse_unary`, which **bypass `parse_expr`**.
3. `parse_prefix_extension` (`expr.rs:225`) - `.b.c.d...`, `[i][i]...`,
   `f()()()`, `:m():m()`. Tail-recursive, and there is no TCO in debug builds.
4. `parse_statements` (`parser.rs:517`) - `do`/`while`/`repeat`/`for`/nested
   `if` bodies, and **function nesting**, since every
   `parse_fndef_named -> parse_chunk -> parse_statements` passes through here.
5. `parse_if_arm` (`stmt.rs:503`) - `elseif` chains, which **bypass both**
   `parse_statements` and `parse_expr` on the chain axis.

Shim pattern, with **no `?` between enter and exit** so the counter decrements
on `Err` as well as `Ok`:

```rust
pub(super) fn parse_expr(&mut self) -> Result<ExpDesc> {
    self.enter_syntax_level()?;
    let result = self.parse_or();
    self.exit_syntax_level();
    result
}
```

Rename each existing body to `*_inner` and wrap. Keep existing
`#[hotpath::measure]` attributes on the outer wrapper and add none elsewhere
(AGENTS.md's recursion warning). A dirty counter cannot leak across parses -
`parse_str_named` builds a fresh `Parser` per call - but the decrement is still
required for within-parse correctness, or 300 *sequential* `do end` blocks
would trip the limit.

**Upvalue resolution needs no separate counter.** `create_parent_upvalue`
(`upvalue.rs:62-98`) recurses once per level of function nesting, so its depth
equals `outer_locals.len()`, already bounded by site 4 above. Each nesting
level costs several `syntax_depth` ticks, so at limit 200 function nesting caps
out around 50-65. It is amplification headroom, not a separate axis. (Note the
#8 cap does not bound this - upvalue count and scope depth are independent.)

**Limit 200**, matching `LUAI_MAXCCALLS` in spirit. The binding constraint is
**not** the 8MB main stack: `cargo test` runs tests on threads with a 2MB
default stack, and the debug gate runs there. Worst case is the paren cycle at
roughly 4-6 native frames per tick; at a pessimistic 1KB/frame in debug, 200
ticks is about 1.2MB, which fits 2MB with margin. Real code rarely exceeds ~50
syntax levels. If the headroom test below ever overflows, lower the constant to
128 rather than raising thread stack sizes.

### D. Tests

In `src/compiler/parser/tests.rs`:

- `line_info_matches_code_len` - recursive helper asserting
  `bc.code.len() == bc.line_info.len()` across all `nested`, over a corpus:
  `print(1)` (the plain-call `remove` path), `local a, b = f()`,
  `a, b, c = f()`, `return f(1)`, a vararg function using `f(...)` and
  `return ...`, `t = {f()}`, `t = {...}`, `obj:m(1)`, `obj:m(f())`. **Fails
  today on the first case.**
- `line_info_reports_correct_line` - a script with ~20 calls before an error on
  line 5; assert the reported line is 5. Today it reports 4.
- `too_many_upvalues_errors` - generated: grandparent with 200 locals, parent
  with 60, inner function referencing all 260; assert `TooManyUpvalues`. Plus a
  positive test at ~250 upvalues that must still parse.
- `syntax_depth_limit` - one generated input per cycle at depth 10_000 (safe
  in-process once the guard exists, since the error fires at 200): nested `(`,
  `do `, unary `-`, `{`, `elseif` chain, `.b` chain. Assert
  `TooManySyntaxLevels` for each.
- `syntax_depth_headroom` - legal scripts just under the limit must parse `Ok`.
  This test *is* the empirical validation that the limit fits the 2MB
  test-thread stack in debug, so it must exercise the **fattest** cycle, not the
  cheapest: statement nesting costs only ~3 small native frames per tick, while
  the paren cycle costs ~13. Include 190 nested `do ... end`, 190 `elseif`, and
  95 nested parens.

  Note parens charge **2 ticks per level** (they descend through both the
  `parse_expr` and `parse_unary` wrappers), so the effective paren limit is ~99,
  not 200, and a 190-paren fixture is rejected rather than accepted. That
  divergence from reference Lua is accepted and documented in the README's
  "Source limits" section; the alternative - charging on the recursion edge
  inside `parse_unary_inner` and `parse_pow` instead of on every `parse_unary`
  entry - is not worth churning the parser for input no legitimate script
  produces.
