Found two defects.

1. **The `ipairs` iterator accepts fractional indices and visits fractional keys.**

   [basic.rs:93](/home/folk/Programs/dellingr/src/lua_std/basic.rs:93) checks only that the control argument is a number, then adds `1.0`. The [generic-for fast path](/home/folk/Programs/dellingr/src/vm/eval_control.rs:209) repeats this behavior.

   ```lua
   local iter = ipairs({})
   print(iter({[2.5] = "fractional"}, 1.5))
   -- Prints: 2.5    fractional
   ```

   This breaks the exact-integer requirement for table positions in [docs/compatibility.md](/home/folk/Programs/dellingr/docs/compatibility.md), and the integer-indexed traversal implied by `ipairs`. Invalid controls silently select a different sequence instead of raising an argument error. NaN, infinity, and numbers outside the signed 64-bit range also pass validation.

   Both paths need the same integer validation; fixing only the library callback leaves generic-for behavior wrong.

2. **Exhausted `gmatch` iterators return one nil value instead of no results.**

   The no-match exits in [string.rs:1467](/home/folk/Programs/dellingr/src/lua_std/string.rs:1467), including the [empty-pattern iterator](/home/folk/Programs/dellingr/src/lua_std/string.rs:1284), explicitly push nil and return one result.

   ```lua
   local iter = string.gmatch("abc", "a", 5)
   print(select("#", iter()))
   -- Prints: 1
   -- Expected: 0
   ```

   [src/lua_std.rs](/home/folk/Programs/dellingr/src/lua_std.rs) promises that `gmatch` yields nothing when `init` exceeds `len + 1`. This also affects ordinary exhaustion. Generic-for hides the difference, but `select`, `table.pack`, and host calls requesting all results observe a phantom result. Collectors using result count to detect exhaustion consequently fail.

   Every no-match exit should return zero results.

Both issues were reproduced through stdin without writing investigation files. Another 5,000 pattern cases agreed with Lua 5.4 after accounting for the second issue. I found no additional confirmed defects in the reviewed scope.
