# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #17, #20, #21, #39: semantic divergences from reference Lua

Four independent, small correctness bugs. Grouped because each is a
self-contained fix and all four are diff-testable against reference.

All verified by running against Lua 5.4.

### #17 (Medium) - `for` control expressions see the loop variable

In Lua the control expressions of a `for` are evaluated in the *enclosing*
scope; the loop variable only exists inside the body. dellingr adds the locals
first and then parses the expressions, so `find_last_local` resolves a name to
the fresh loop slot.

- numeric: `stmt.rs:230-244` - `add_local("")` x3, `add_local(name)`, *then*
  `parse_expr()` for start/stop/step.
- generic: `stmt.rs:291-316` - the three hidden locals plus every visible name
  are added, *then* `parse_explist()`.

**Verified:**

```lua
local i = 5
for i = i, 7 do print(i) end
```

Reference prints `5 6 7`. dellingr raises
`attempt to perform arithmetic on a nil value` - the start expression read the
new, still-nil slot.

Worse than the error suggests: slot reuse can make this silently *wrong*
rather than an error. If an earlier scope used the same slot index, the stale
value is read as the start value, giving wrong iteration bounds with no
diagnostic. The generic form has the same shape:
`for _, t in ipairs(t) do ... end`.

Fix direction: parse the control expressions first, then add the hidden and
visible locals. The emitted slot arithmetic already targets `locals.len()`
-relative slots, so record the base index before parsing and add the locals
after.

### #20 (Medium) - NaN `<=` and `>=` evaluate true

`eval_compare` (`src/vm/eval_store.rs:484-509`) maps `partial_cmp -> None` to
`Ordering::Equal`, and `frame.rs:277-278` implements `<=` as negated `>` and
`>=` as negated `<`. For NaN the result is `Equal`, which is neither
`Greater` nor `Less`, so the negated forms return true.

**Verified:**

| expression | dellingr | reference |
|---|---|---|
| `(0/0) <= 1` | true | false |
| `(0/0) >= 1` | true | false |
| `1 <= 0/0` | true | false |
| `(0/0) < 1` | false | false |

`<` and `>` are already correct, since `Equal` matches neither target.

Fix: on `partial_cmp() == None`, push false unconditionally *before* the
negate step - or compute `<=` as a first-class comparison rather than `!(>)`.

### #21 (Medium) - floored modulo gives NaN for an infinite divisor

`OP_MOD` (`src/vm/frame.rs:328-333`) computes `a - (a/b).floor() * b`. With
`b = inf`: `a/b` is 0, and `0 * inf` is NaN.

**Verified:**

| expression | dellingr | reference |
|---|---|---|
| `1 % (1/0)` | NaN | 1.0 |
| `-1 % (1/0)` | NaN | inf |

The formula is also less exact than `fmod` for large finite operands, where
rounding in `a/b` can flip the floor.

Fix: implement as reference's `luai_nummod` does -
`m = a.rem(b)` (fmod), then `if m != 0 && (m < 0) != (b < 0) { m += b }`.

### #39 (Medium) - `math.modf(+-inf)` returns a NaN fractional part

`src/lua_std/math.rs:323` uses `x.fract()`, which is `x - x.trunc()`, so
`inf - inf` is NaN.

**Verified:**

| expression | dellingr | reference |
|---|---|---|
| `math.modf(1/0)` | `inf`, `NaN` | `inf`, `0.0` |
| `math.modf(-1/0)` | `-inf`, `NaN` | `-inf`, `0.0` |

Reference returns 0.0 for the fractional part (5.2 via C `modf`, 5.4 via an
explicit `n == ip` test).

---

## Agreed implementation plan

Correction to the above: **Lua 5.2 also returns NaN** for `1 % inf` - its
`luai_nummod` is the old direct floor formula, and fmod-plus-adjust is the 5.4
implementation. For #39, 5.2 returns *signed* zero (`-0` for `-inf`) while 5.4
explicitly returns positive `0.0`. So both are 5.4-targeted changes.

That matters for testing: `diff_test.sh` passes a file when dellingr matches
**either** 5.2 or 5.4, whole-file. A modulo example on its own would therefore
pass before *and* after the fix and enforce nothing. Each new example must
include something only 5.4 produces, so the whole file is forced to match 5.4.

### #17 - `for` control scope

Record the base, parse all control expressions, then declare the locals. This
is sufficient for both forms. Slot layout is unchanged: numeric keeps controls
at `base..base+2` and the visible variable at `base+3`; generic keeps controls
at `base..base+2` and visible variables at `base+3..`.

Nothing else depends on declaration timing: the per-iteration close stays at
`base + 3`, `enter_loop(base)` still makes `break` close every loop-owned slot,
`exit_loop` still patches jumps before `level_down` emits the final close, and
`FOR_PREP` / `FOR_LOOP` / `TFOR_PREP` / `TFOR_CALL` / `TFOR_LOOP` keep exactly
the same operands. Expression parsing adds no persistent locals - function
literals replace and restore the outer locals vector in `parse_chunk`
(`parser.rs:491`), so nested closures cannot move the saved base.

Bytecode stability follows because `add_local` emits no instructions or
literals; it only updates `locals` and `num_locals`. Any program whose control
expressions do not mention a loop-variable name compiles byte-identically.

Add a non-mutating `ensure_local_capacity(additional)` preflight - 4 slots for
numeric, `3 + names.len()` for generic - so the existing "too many locals"
error precedence is preserved without declaring anything early.

### #20 - NaN ordered comparisons

`eval_compare` (`eval_store.rs:480`) is reached only by the four ordered
opcodes in `frame.rs:273`, and has exactly three paths: number/number via
`partial_cmp` (where `None` means NaN), string/string total byte ordering
(never `None`), and the existing type error. There is no comparison-metamethod
path - those are deliberately unsupported - and equality uses separate arms.

So: represent the result as `Option<bool>`, apply `negate` only inside `Some`,
and map `None` to false. String ordering and type errors are untouched.

### #21 - Lua modulo

Neither Rust operator alone works: `%` is fmod (sign follows the dividend) and
needs correction when the divisor has the opposite sign, while `rem_euclid`
always seeks a non-negative result and is wrong for negative divisors
(Lua requires `5 % -3 == -1`).

Use `%` plus Lua 5.4's exact two-branch predicate: add `b` when
`m > 0 && b < 0`, or when `m < 0 && b > 0`; otherwise keep `m`. Prefer this
spelling over the approximate `m != 0 && sign-mismatch` form, because the
reference predicate avoids an unnecessary addition when `m` is NaN.

Private helper in `frame.rs`, called from `OP_MOD`. Cost stays exactly 1.

### #39 - `math.modf`

`open_math` (`math.rs:317`): compute `integral = x.trunc()` once, then use
`fractional = 0.0` when `x == integral` and `x - integral` otherwise. That
mirrors 5.4's infinity guard and deliberately yields positive zero.

### Tests

No existing test or example asserts any of the four wrong results, so nothing
needs updating. `test21` (`parser/tests.rs:586`) pins numeric-for bytecode
exactly and must keep passing - it is the regression guard for #17.

**Do not** put the new numeric cases in `edge_cases.lua` or `feature_test.lua`:
both carry file-wide `-- DIFF:` markers, which would silently remove them from
differential enforcement.

New `examples/for_control_scope.lua`, untagged:

- numeric collision with a deliberately stale reused slot, which today yields a
  wrong value rather than throwing;
- generic collision with a stale table in the reused visible-variable slot;
- `name: true/false` assertions so `run_examples` and both reference
  interpreters cover it.

New `examples/lua54_numeric_edges.lua`, untagged:

- every ordered comparison with NaN on both sides;
- string `<=`/`>=` guards, to prove string ordering did not change;
- positive and negative infinite-divisor modulo, plus finite opposite-sign;
- `math.modf(+-inf)`, including `1 / fractional == math.huge` to pin *positive*
  zero;
- **and a 5.4-only signature so the whole file must match 5.4** rather than
  being satisfied by 5.2's old modulo behaviour.

`src/compiler/parser/tests.rs`: keep `test21`; add an exact-bytecode fixture
for an ordinary generic loop; add a nested-function control-expression test
asserting a same-named reference captures the enclosing local rather than the
future loop slot.

`src/vm/frame.rs` unit tests: the modulo helper directly, for finite opposite
signs, both infinite-divisor signs, NaN propagation, and signed zero.

### Superseded questions

1. For #17, is "record the base index, parse the control expressions, then add
   the locals" actually sufficient for **both** the numeric and generic forms?
   The generic form adds three hidden locals plus an arbitrary name list, and I
   want to know whether the emitted slot arithmetic, the close-upvalue
   boundaries, or the break-jump bookkeeping depend on the locals existing
   before the expressions are parsed. This is the one with real regression
   potential.
2. For #20, is pushing false on `None` correct for **every** comparison
   operator that routes through `eval_compare`, including any metamethod or
   string-comparison path? Reference makes every ordered comparison involving
   NaN false, but I want the change scoped to numeric NaN rather than
   accidentally changing string ordering.
3. For #21, does `f64::rem_euclid` or plain `%` in Rust already give reference
   semantics, or is the explicit fmod-plus-adjust the only correct route? Rust's
   `%` on floats is fmod, so the adjust step is presumably still needed for
   sign.
4. Do any of these four have existing tests or examples that assert the
   *current* wrong behaviour and would need updating? #20 in particular feels
   like something a test might have pinned.

Read `src/compiler/parser/stmt.rs`'s numeric and generic `for` handling,
`src/vm/eval_store.rs`'s `eval_compare`, `src/vm/frame.rs`'s comparison and
`OP_MOD` arms, and `src/lua_std/math.rs`.
