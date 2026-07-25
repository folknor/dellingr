//! Tests for method-lookup inline caches w.r.t. `globals_version`.
//!
//! Both `StringMethodCacheEntry` and `MethodLookupCacheEntry` embed a
//! direct `ObjectPtr` into a global library table (the `string` lib for
//! the former, whatever `__index` resolved to for the latter). If a
//! builtin global is rebound from Lua, or `with_restricted_env` swaps
//! the environment, the original library can stay alive (still rooted
//! by the IC and by `saved_builtins`), so without a `globals_version`
//! check the cache silently bypasses the new binding. These tests pin
//! the invalidation behavior.

use dellingr::{ArgCount, RetCount, State};

fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

#[test]
fn string_method_cache_invalidates_on_string_rebind() {
    let val = run_number(
        r#"
        local function f() return ("a"):upper() == "A" and 1 or 0 end
        local first = f()
        string = { upper = function(_self) return "REBOUND" end }
        local second = f()
        local result_after = ("a"):upper() == "REBOUND" and 1 or 0
        return first * 100 + second * 10 + result_after
    "#,
    );
    // first: cache cold, hits real string lib -> 1
    // second: cache previously warm, but globals_version bumped -> slow
    //         path resolves through new `string` -> "REBOUND" so == "A"
    //         is false -> 0
    // result_after: direct slow-path call with new string lib -> 1
    assert_eq!(val, 101.0);
}

#[test]
fn string_method_cache_survives_gc_after_string_rebind() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            function warm() return ("a"):upper() end
            function rebind_and_drop()
                string = { upper = function(_self) return "NEW" end }
            end
            function probe() return ("a"):upper() end
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Warm the string-method IC at the call site inside `warm`.
    state.get_global("warm");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    state.pop(1).unwrap();

    // Rebind `string` in Lua and drop the local reference.
    state.get_global("rebind_and_drop");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Force GC so the original `string` lib (no longer rooted via the
    // builtins slot) is collected. With the IC fix the cache rejects
    // the stale entry on globals_version mismatch and the slow path
    // resolves through the new `string` table; without the fix this
    // would either return stale data or panic on a stale ObjectPtr.
    state.gc_collect();

    state.get_global("probe");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let result = state.to_string(-1).unwrap();
    assert_eq!(result, "NEW");
}

#[test]
fn string_method_cache_blocked_by_with_restricted_env() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            function call_upper() return ("hi"):upper() end
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Warm the IC outside the sandbox.
    state.get_global("call_upper");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let warm = state.to_string(-1).unwrap();
    assert_eq!(warm, "HI");
    state.pop(1).unwrap();

    // Inside the sandbox, whitelist the entry-point function but NOT
    // `string`. With the fix, the IC's globals_version mismatch forces
    // the slow path, which sees Nil for the `string` builtin and
    // errors. Without the fix, the cached ObjectPtr (still alive in
    // `saved_builtins`) silently bypasses the restriction.
    let restricted_result = state.with_restricted_env(&["call_upper"], |state| {
        state.get_global("call_upper");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1))
    });
    assert!(
        restricted_result.is_err(),
        "with_restricted_env without `string` whitelist must reject `s:upper()`, \
         got: {restricted_result:?}"
    );

    // After the sandbox returns, the IC works again on a fresh call.
    state.get_global("call_upper");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let after = state.to_string(-1).unwrap();
    assert_eq!(after, "HI");
}
