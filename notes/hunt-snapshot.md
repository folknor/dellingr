Found two defects.

1. **[P1] Load loses rendered identities of literals in nested functions without live closures.** [Materialization](/home/folk/Programs/dellingr/src/vm/save_state.rs:1709) creates runtime bundles only for saved closure objects. Nested bytecode is rebuilt, but its previously populated runtime is omitted.

   Reproducer: execute this chunk, return to the host, then save/load with no setup:

   ```lua
   function make()
       return function() return "nested-only" end
   end
   before = string.format("%p", make()())
   ```

   The continuous State retains the nested function's runtime through `make`'s bytecode tree. [Capture](/home/folk/Programs/dellingr/src/vm/save_state.rs:856) therefore records the literal's rendered identity. During load, however, the temporary nested closure has no saved object, so its runtime is never reconstructed. The [final collection](/home/folk/Programs/dellingr/src/vm/save_state.rs:1875) frees the otherwise unrooted literal and prunes its identity.

   Consequently, `string.format("%p", make()())` produces a new identity after load while the continuous State retains `before`. An immediate resave also loses the identity record. This breaks the [save-state contract](/home/folk/Programs/dellingr/docs/save-state.md) for rendered identities and continued byte stability. Restoring these identities must also restore their lifetime through reachable nested bytecode.

2. **[P2] Gmatch validation rejects legitimate ordinary closures.** [Iterator detection](/home/folk/Programs/dellingr/src/vm/save_state/verify.rs:742) assumes that any closure capturing `string.gmatch.iter` or `string.gmatch.empty_iter` is a built-in iterator wrapper.

   The public `registered_rust_fn` API exposes those callbacks. Push either handle as global `seed`, then execute:

   ```lua
   local f = seed
   keep = function() return f end
   ```

   Saving succeeds. Loading rejects the ordinary `keep` closure with `GmatchCorruption::StateTableMissing`, before setup runs. The same failure occurs in a raw State that legitimately registers a host function under either id.

   This breaks the documented persistence of closures and registered Rust functions, including the explicit permission for raw States to register stock ids. A captured registration id alone cannot establish that a closure owns the gmatch state schema.

These findings are source-traced. The existing snapshot integration suite passed, including the current released-format archive check. No source files were changed.
