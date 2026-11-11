Found two behavioral defects and one documentation defect.

1. **[P2] Interrupted Rust metamethod admission consumes host operands before the handler runs.**

   The broken claim is the pre-entry atomicity contract in [docs/limits.md](/home/folk/Programs/dellingr/docs/limits.md) and the host table methods' documentation.

   Reproducer: obtain `setmetatable({}, {__index = print, __newindex = print})`, push a missing key, trigger the State's interrupt, then call `get_table(1)`. It returns `Interrupted` with the key removed. Independently, `set_table(1)` removes both the key and value under the same conditions. Neither handler executes.

   [begin_call](/home/folk/Programs/dellingr/src/vm/eval.rs:449) marks a Rust handler entered before [invoke_rust_fn](/home/folk/Programs/dellingr/src/vm/eval.rs:662) polls the interrupt. The [read cleanup](/home/folk/Programs/dellingr/src/vm/table_ops.rs:324) and [store cleanup](/home/folk/Programs/dellingr/src/vm/table_ops.rs:508) consequently treat admission refusal as an entered-handler failure. Lua handlers poll before marking entry and preserve the operands.

   Entry should be committed after the interrupt check, immediately before the Rust body runs.

2. **[P2] A compilation error propagated through a Rust callback displays the caller's location instead of the rejected source's.**

   This breaks the documented syntax-error location contract in [docs/compatibility.md](/home/folk/Programs/dellingr/docs/compatibility.md) and produces a misleading diagnostic.

   Reproducer: register a Rust callback that returns the error from `Program::compile("local =", Some(b"child.lua"))`, then invoke it on line 3 of `outer.lua`. The error retains `child.lua`, line 1 and its column through the structured accessors, but its displayed prefix becomes `outer.lua:3:`. The rejected source's location disappears from the diagnostic.

   [locate_error](/home/folk/Programs/dellingr/src/vm/eval.rs:1161) preserves the original location and adds a Lua traceback. [Error::fmt](/home/folk/Programs/dellingr/src/error.rs:1667) then unconditionally prefers that traceback's first frame for ordinary errors.

   Explicit primary diagnostic locations should survive traceback attachment; `ScriptError`'s deliberate frame-selection policy can remain separate.

3. **[P3] `RawField` promises integer implementations it does not provide.**

   Its [public documentation](/home/folk/Programs/dellingr/src/vm/raw_field.rs:47) says it supports “every fixed-width integer type,” but implementations cover only signed and unsigned 8-, 16-, 32- and 64-bit integers. `raw_field::<i128>` and `raw_field::<u128>` fail to compile, including their `Option` forms.

   This can be corrected by listing the supported widths explicitly; adding types is unnecessary to resolve the documentation defect.

These findings are source-traced, not runtime-tested. I made no filesystem changes.
