I would prioritize three rewrites: matcher execution, formatting and number rendering, and gmatch iteration. The current implementation already compiles and caches patterns and borrows pattern arguments without copying whole strings. The remaining opportunities are further down the execution path.

This is a source-only assessment. I read implementation and test code, including the relevant heap, call, table, and snapshot paths. I did not read project Markdown, inspect history, edit files, build, or run benchmarks. Rankings below are hypotheses about payoff, not measured speedups.

1. **Rewrite matcher execution around larger, precisely costed kernels. Highest overall ceiling.**

   The bottleneck is no longer pattern parsing. It is executing a small interpreter for almost every subject byte.

   In [luapat.rs](/home/folk/Programs/dellingr/src/patterns/luapat.rs:598), an ordinary atom goes through `match_at`, charges an item step, dispatches `Item::Atom`, calls `singlematch`, charges another step, fetches a class, and tests membership. Each charge polls the interrupt flag and enters the meter's mode-dependent accounting. Repetition avoids some item dispatch but retains per-byte charging and polling. Unanchored search restarts this machinery at every position.

   Compilation also erases useful distinctions: literal bytes, `.`, and arbitrary classes all become indexed bitsets. That simplifies scalar execution but prevents the execution plan from directly expressing a literal run, a delimiter scan, or a specialized ASCII run.

   **The stronger redesign:** replace the current item executor with a compiled execution plan containing literal runs, specialized class scans, repetition kernels, and explicit backtracking continuations. Retain scalar instructions for frontier tests, balance, backreferences, captures, and deferred failures where necessary. Execute deterministic stretches as stretches, rather than interpreting every atom independently.

   The key is to attach the *current logical cost* to these kernels. For example, searching a digit-free subject of length `N` with `%d` currently performs `2 * (N + 1)` matcher steps, including the failed attempt at the end. A specialized scan can find the first digit and account for rejected positions arithmetically. Literal runs can count matching-prefix steps; repetitions can count accepted bytes and the terminating failed test.

   [CostMeter](/home/folk/Programs/dellingr/src/cost_meter.rs:304) already provides the important primitive: `consume_units_admitted` preserves unit-by-unit admission, including allowing the crossing unit and rejecting the next. The matcher mostly uses it with one unit. Larger kernels should exploit it directly, resolving a budget boundary within the kernel before publishing captures or executing a later failure.

   Move accounting mode selection outside the byte loop. Count-only execution can accumulate logical steps locally; finite-budget execution can track the next admission boundary; zero-weight execution need not maintain counters. Flush pending accounting on success, failure, pattern error, and interruption. Keep interrupt checks at bounded kernel boundaries and throughout backtracking.

   **Why this is plausibly large:** it attacks work multiplied by subject length across find, match, gmatch, and gsub. Failed scans, token extraction, quoted fields, and repeated classes all appear in the Lua workloads. It removes dispatch and serialized counter updates, not merely a function-call instruction.

   **Risks:** exact backtracking order, capture values, deferred malformed-pattern errors, and match-depth errors are observable. An explicit continuation stack must preserve the existing *logical* depth rules even after native recursion disappears. Prefix skipping must prove what the skipped attempts would have charged and whether they could have errored.

   I would do this rewrite. I would not substitute a conventional regex engine: identical matches do not imply identical costs, error precedence, or depth failures. Nor would I promise a general linear-time matcher while preserving the existing backtracking bill; reconstructing that bill could consume the algorithmic savings.

2. **Rewrite formatting as a cached execution plan writing directly to one result buffer. High conviction.**

   [string_format.rs](/home/folk/Programs/dellingr/src/lua_std/string_format.rs:57) currently has a materialization pipeline:

   - Copy the entire format argument.
   - Parse each directive and allocate its `raw` bytes.
   - Render the argument into an owned vector.
   - Frequently create additional digit, precision-padding, or float-body buffers.
   - Copy the directive result into the aggregate output.
   - Intern the aggregate through an interface that copies again on a miss.

   Even `%s` without a metamethod copies its selected bytes into `pad_bytes`, then copies that result into the output. Integer formatting separates digit generation, precision padding, field assembly, and final assembly into owned buffers.

   **The stronger redesign:** compile an interned template into literal spans, escaped-percent operations, typed directives, and deferred error operations. Cache that plan by string identity. Execute it directly into one checked output buffer. Write padding and signs directly; generate numeric digits into bounded scratch storage; append borrowed `%s` ranges directly.

   This should replace the current formatting pipeline, not add a cache around it. Cached parsing alone leaves most allocation and copying intact.

   Preserve execution ordering in the plan. Today missing-argument detection precedes directive parsing; validation precedes the directive charge; the charge precedes `__tostring`; literal appends and `%` polls have their own positions. A cached plan must encode those events rather than validate the complete template eagerly.

   The format argument can remain rooted in its stack slot across `__tostring`, with literal spans reacquired after re-entry. Copying the whole template is unnecessary. Similarly, a string returned by `__tostring` should remain a rooted value until its bytes have been appended, rather than being popped and converted into an owned copy in [tostring_rendering](/home/folk/Programs/dellingr/src/vm/table_ops.rs:1013).

   **Number rendering deserves a real rewrite within this work.** Both [format_g14](/home/folk/Programs/dellingr/src/vm/lua_val.rs:75) and [format_general](/home/folk/Programs/dellingr/src/lua_std/string_format.rs:626) format scientific text to discover the rounded exponent, parse that exponent back into an integer, then construct the final representation. `format_general` can perform another scientific conversion after its probe.

   Replace the text probe with a decimal conversion result containing rounded digits and an exponent. Render fixed or scientific notation from that result. Preserve exact significant-digit rounding, exponent thresholds and spelling, negative zero, NaN normalization, alternate form, and precision behavior. This is precision-directed conversion; a shortest-round-trip formatter is not a substitute.

   The existing small-integral fast path in `number_to_string` is already valuable. Keep its semantics, but let it write into inline storage or the final sink without allocating a `String`.

   **Why this is plausibly large:** repeated game templates reuse syntax while varying only arguments. This redesign removes repeated parsing, several allocations per directive, large `%s` copies, and duplicate numeric conversions. The rendering improvement also reaches tostring, numeric string arguments, and concatenation.

   **Risks:** decimal rounding boundaries are the main numerical risk. Callback ordering and exact string-size errors are the main pipeline risks. Streaming output must retain the current logical append boundaries where they determine the reported attempted size or which error wins.

3. **Replace gmatch's execution machinery with a native iterator operation. High conviction for many short matches.**

   The per-yield overhead in [gmatch_iter](/home/folk/Programs/dellingr/src/lua_std/string.rs:1199) is substantial:

   - Execute a Lua wrapper that calls the Rust iterator.
   - Push and intern field-name strings.
   - Read subject, pattern, position, and previous-match fields through stack-based table APIs.
   - Validate and convert numeric state.
   - Look up the compiled pattern and create fresh matcher state.
   - Allocate a capture vector.
   - Push captures and stage two table writes.
   - Remove the state and argument slots beneath the results.

   The empty-pattern iterator does much of the table and stack work without any matching work to amortize it.

   **The stronger redesign:** give a genuine gmatch closure an intrinsic execution form with a typed runtime record: compiled program, subject reference, field-slot references, and bounded capture storage. Enter that operation directly instead of executing the wrapper and its nested callback.

   The snapshot contract constrains persistence, not execution. Keep the exact saved closure, upvalues, table fields, insertion order, and values described by [gmatch_schema.rs](/home/folk/Programs/dellingr/src/gmatch_schema.rs:1). Use them as the persisted representation while executing through a runtime view. Update position fields directly after successful result construction.

   There is already useful machinery for this: [Table slot tokens](/home/folk/Programs/dellingr/src/vm/table.rs:1055) support validated direct reads and writes. Gmatch currently reaches those fields through general stack APIs instead. Holding the compiled program also avoids repeated cache lookup and protects an active iterator from cache eviction.

   Recognition must check the actual callable and captured values. The snapshot loader's wrapper recognition is explicitly only a diagnostic heuristic; matching a saved function ID or source name is insufficient authority to replace execution. Registered iterator handles must continue accepting arbitrary host-supplied tables and producing the existing validation errors.

   **Why this is plausibly large:** for short words or fields, surrounding iterator work can rival matching itself. This removes an entire execution handoff and the repeated reconstruction of private state.

   **Risks:** preserve closure identity and rendering, byte-identical snapshots, error attribution, result counts, and iterator position after budget or stack refusal. Removing wrapper frames must also preserve relevant stack-cap refusals. The direct entry belongs partly to the calls scope; it need not wait for a universal Rust-closure API. Afterwards, if this works, generalizing captured native callables is a follow-up.

4. **Rewrite pure gsub replacement as one borrowed scan-and-write operation. High conviction, narrower coverage.**

   Gsub already compiles its replacement template once per call. The remaining seam is between matching, capture materialization, replacement dispatch, subject access, and output construction.

   In [gsub_lua_pattern](/home/folk/Programs/dellingr/src/lua_std/string.rs:992), each accepted match copies capture descriptions into a vector, enters replacement dispatch, and repeatedly resolves subject ranges through `State`. Template compilation first copies replacement bytes, then allocates owned literal segments.

   **The stronger redesign:** select the replacement mode once before scanning. For string or numeric replacement, acquire the compiled pattern, a replacement plan, subject bytes, replacement bytes, meter, interrupt flag, and output buffer together. Run the entire substitution loop over those borrows. Consume captures directly from matcher state. Store replacement literals as spans, with explicit operations for escaped percent and capture references.

   Cache replacement plans for repeated replacement strings. Cache malformed escapes as deferred failures; they must still error only when replacement execution reaches them.

   Keep function and table replacement as separate re-entry paths. They require suspending the scan and maintaining roots. They do not justify routing pure replacement through a callback-capable interface on every match. In those paths, append returned string bytes while the return value is still rooted, rather than cloning the value into a temporary vector before appending.

   **Why this is plausibly large:** substitutions over identifiers, log fields, tags, and delimiters commonly produce many matches with tiny replacements. The pure path can eliminate per-match capture-vector materialization, repeated argument resolution, and replacement-mode dispatch.

   **Risks:** unfinished captures error only when consumed; empty-match suppression and anchored behavior must remain exact. Preserve callback effects on later failure. A global output-size preflight cannot run ahead of callbacks or deferred template errors and change which failure wins.

5. **Remove the owned-buffer-to-borrowed-interner handoff. A required heap-scope dependency.**

   [push_bytes](/home/folk/Programs/dellingr/src/vm/stack.rs:144) accepts `AsRef<[u8]>`; passing a `Vec` transfers no ownership to the pool. [alloc_string_raw](/home/folk/Programs/dellingr/src/vm/object.rs:642) hashes and probes the bytes, then allocates and copies them into a box on a miss.

   Consequently upper/lower/reverse, format, gsub, and concatenation build a complete owned result only to copy it again. [concat_values](/home/folk/Programs/dellingr/src/vm/eval.rs:771) additionally keeps separately allocated number renderings before copying them into its exact-sized buffer.

   **What I want from the heap scope:** an owned-buffer adoption path with the same content-based interning, allocation policy, roots, and refusal guarantees. On a miss, adopt storage; on a hit, return the existing identity and discard the candidate. Avoid a shrink-to-fit design that silently restores the full copy.

   Afterwards, if this works, the construction pattern can become a common checked builder. Designing that general abstraction first is unnecessary.

   This removes a result-sized copy and allocation on misses. It does not remove hashing, equality checks, or candidate construction on hits. Returning an existing string directly remains better where possible.

The medium-value local changes are worthwhile, but I would keep them separate from those rewrites:

| Change | Opportunity and limit |
|---|---|
| Make `sub` use `push_bytes_from_stack_range` | The existing API already interns directly from rooted source bytes. `sub` instead allocates a vector first. Preserve argument and charge ordering. |
| Return unchanged results directly | Gsub currently allocates subject-sized output even when nothing matches. Upper/lower can reuse the input when no byte changes. Retain required scan costs, errors, and interrupt observation. |
| Eliminate temporary replacement-value copies | Function and table gsub replacements can append borrowed stack-string bytes before popping them. |
| Use bounded capture storage | Gmatch allocates a vector per successful yield; gsub materializes capture descriptions per match. Captures have a fixed maximum. Useful locally, but the larger execution rewrites remove the handoff entirely. |
| Fuse numeral validation and recognition | [numeral.rs](/home/folk/Programs/dellingr/src/numeral.rs:88) scans decimal syntax, validates it, validates UTF-8, then invokes Rust parsing. A byte parser with a conservative exact-small-integer path can reduce passes. Parsing already borrows its input, so this has less structural headroom than rendering. |
| Improve literal-search kernels | [scan_subslice](/home/folk/Programs/dellingr/src/lua_std/string.rs:123) skips first-byte mismatches but still compares long shared prefixes repeatedly. Faster equality/prefix kernels are plausible. A stronger search algorithm must also reconstruct the existing naive comparison bill; changing the search algorithm alone is insufficient. |

For explicit-base tonumber, preserve its successive floating-point accumulation. Replacing it with integer accumulation and one final conversion can change long-input results. For len on strings and tostring on strings without a handler, the implementation already avoids byte copying; their remaining significant opportunity mostly belongs to the calls scope.

Two source findings deserve attention during implementation:

- **A failed gmatch search does not record exhaustion.** Repeated calls can rescan the remaining suffix and recharge the search. This is avoidable host work, but simply adding an exhausted flag changes costs and saved state. Any memoization must preserve both.
- **The pattern cache's byte limit counts source bytes, not compiled storage.** Items, class tables, vector capacity, and ownership overhead are additional. An oversized pattern bypasses caching, so repeated gmatch iterations can recompile it. Holding the compiled program in the iterator removes that particular repeated-work path.

I did not establish a definite correctness bug from these reads. The strongest throughput hypothesis is that substantial work remains removable without changing results: execute pattern stretches rather than individual atoms, render directly rather than through temporary text, and stop rebuilding iterator state on every yield.
