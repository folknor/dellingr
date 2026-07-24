//! Tests for GC handling of closed upvalues in closures.
//!
//! These tests verify that values captured in closures survive garbage collection
//! even after the original scope has ended and the upvalue has been "closed"
//! (moved from the stack to the upvalue pool).

use dellingr::{ArgCount, RetCount, State};

fn install_force_gc(state: &mut State) {
    state.push_rust_fn(|state| {
        state.gc_collect();
        Ok(0)
    });
    state.set_global("force_gc");
}

fn install_force_next_gc(state: &mut State) {
    state.push_rust_fn(|state| {
        state.gc_set_threshold(1);
        Ok(0)
    });
    state.set_global("force_next_gc");
}

#[test]
fn tombstoned_keys_and_values_are_not_gc_roots() {
    let mut state = State::new();
    state.gc_disable_auto();
    state.load_string("t = {}").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    state.gc_collect();
    let baseline = state.object_count();

    state
        .load_string("local key = {}; local value = {}; t[key] = value; t[key] = nil")
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    state.gc_collect();
    assert_eq!(state.object_count(), baseline);
}

#[test]
fn active_frame_string_literal_survives_explicit_gc() {
    let mut state = State::new();
    state.gc_disable_auto();
    install_force_gc(&mut state);

    state
        .load_string(
            r#"
        force_gc()
        return "literal after gc"
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_string(-1).unwrap(), "literal after gc");
    state.pop(1);
}

#[test]
fn string_literals_are_unrooted_after_frame_exits() {
    let mut state = State::empty();
    state.gc_disable_auto();

    assert_eq!(state.string_count(), 0);
    state
        .load_string(
            r#"
        local temporary = "collect this literal after return"
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    assert!(state.string_count() > 0);
    state.gc_collect();
    assert_eq!(state.string_count(), 0);
}

#[test]
fn open_upvalue_survives_gc_while_defining_frame_is_active() {
    let mut state = State::new();
    state.gc_disable_auto();
    install_force_gc(&mut state);

    state
        .load_string(
            r#"
        local function outer()
            local captured = {value = 55}
            live = function()
                return captured.value
            end
            force_gc()
            return live()
        end

        return outer()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_number(-1).unwrap(), 55.0);
    state.pop(1);
}

#[test]
fn executing_returned_closure_roots_closed_upvalues_during_auto_gc() {
    let mut state = State::empty();

    state
        .load_string(
            r#"
        local function make_closure()
            local captured = {value = 42}
            return function()
                local trigger_gc = {}
                return captured.value
            end
        end

        return make_closure()()
    "#,
        )
        .unwrap();
    state.gc_set_threshold(1);
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1);
}

#[test]
fn temporary_callable_table_survives_gc_during_call_metamethod_lookup() {
    let mut state = State::new();
    install_force_next_gc(&mut state);

    state
        .load_string(
            r#"
        local function make_callable()
            local callable = setmetatable({ value = 73 }, {
                __call = function(self)
                    return self.value
                end
            })
            force_next_gc()
            return callable
        end

        return make_callable()()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_number(-1).unwrap(), 73.0);
    state.pop(1);
}

/// Basic test: closure captures a table, table survives GC.
#[test]
fn closure_captured_table_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto(); // Manual GC control

    // Create a closure that captures a local table
    state
        .load_string(
            r#"
        local function make_closure()
            local captured = {value = 42}
            return function() return captured.value end
        end
        test_fn = make_closure()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Force GC - the captured table should survive
    state.gc_collect();

    // Call the closure - should still work
    state.get_global("test_fn");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 42.0, "Captured table value should survive GC");
    state.pop(1);
}

/// Test multiple GC cycles with closure holding table.
#[test]
fn closure_survives_multiple_gc_cycles() {
    let mut state = State::new();
    state.gc_disable_auto();

    state
        .load_string(
            r#"
        local function make_closure()
            local data = {count = 0}
            return function()
                data.count = data.count + 1
                return data.count
            end
        end
        counter = make_closure()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Call and GC multiple times
    for expected in 1..=5 {
        state.gc_collect();

        state.get_global("counter");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

        let result = state.to_number(-1).unwrap();
        assert_eq!(
            result, expected as f64,
            "Counter should be {expected} after GC cycle"
        );
        state.pop(1);
    }
}

/// Test closure capturing another closure (nested upvalues).
#[test]
fn nested_closures_survive_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    state
        .load_string(
            r#"
        local function outer()
            local x = 10
            local function middle()
                local y = 20
                return function()
                    return x + y
                end
            end
            return middle()
        end
        nested_fn = outer()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.gc_collect();

    state.get_global("nested_fn");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 30.0, "Nested closure should access both upvalues");
    state.pop(1);
}

/// Test metatable stored as upvalue (the original bug case).
#[test]
fn metatable_as_upvalue_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    // This pattern mirrors fleet:group() - metatable defined once, used by closure
    state
        .load_string(
            r#"
        local mt = {
            __index = {
                get_value = function(self) return self._value end
            }
        }

        function create_object(val)
            local obj = {_value = val}
            setmetatable(obj, mt)
            return obj
        end
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Force GC - mt should survive because create_object closure references it
    state.gc_collect();

    // Create an object using the closure
    state.get_global("create_object");
    state.push_number(99.0);
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    state.set_global("obj");

    // Access via metatable method
    state.load_string("return obj:get_value()").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 99.0, "Metatable method should work after GC");
    state.pop(1);
}

/// Test that unreferenced closures ARE collected.
#[test]
fn unreferenced_closure_is_collected() {
    let mut state = State::new();
    state.gc_disable_auto();

    let size_before = state.heap_size();

    // Create and immediately discard a closure with captured table
    state
        .load_string(
            r#"
        local function make_and_discard()
            local big_table = {a=1, b=2, c=3, d=4}
            local fn = function() return big_table end
            -- fn goes out of scope here, not stored anywhere
        end
        make_and_discard()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    let size_after_create = state.heap_size();
    assert!(
        size_after_create > size_before,
        "Heap should grow after creating objects"
    );

    state.gc_collect();

    let size_after_gc = state.heap_size();
    assert!(
        size_after_gc < size_after_create,
        "GC should collect unreferenced closure and table. Before GC: {size_after_create}, After: {size_after_gc}"
    );
}

/// Test closure with multiple upvalues of different types.
#[test]
fn closure_with_mixed_upvalues() {
    let mut state = State::new();
    state.gc_disable_auto();

    state
        .load_string(
            r#"
        local function make_closure()
            local num = 42
            local str = "hello"
            local tbl = {x = 1}
            local fn = function() return "inner" end

            return function()
                return num, str, tbl.x, fn()
            end
        end
        mixed_fn = make_closure()
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.gc_collect();

    state.get_global("mixed_fn");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();

    assert_eq!(state.to_number(-4).unwrap(), 42.0);
    assert_eq!(state.to_string(-3).unwrap(), "hello");
    assert_eq!(state.to_number(-2).unwrap(), 1.0);
    assert_eq!(state.to_string(-1).unwrap(), "inner");
    state.pop(4);
}

/// Test closure stored in a table survives GC.
#[test]
fn closure_in_table_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    state
        .load_string(
            r#"
        local registry = {}

        local function register(name, value)
            local captured = value
            registry[name] = function() return captured end
        end

        register("answer", 42)
        register("pi", 3.14159)

        function get(name)
            return registry[name]()
        end
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.gc_collect();

    // Check "answer"
    state.get_global("get");
    state.push_string("answer");
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1);

    state.gc_collect();

    // Check "pi"
    state.get_global("get");
    state.push_string("pi");
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    let pi = state.to_number(-1).unwrap();
    assert!((pi - std::f64::consts::PI).abs() < 0.0001);
    state.pop(1);
}

/// Stress test: many closures with shared upvalue.
#[test]
fn many_closures_shared_upvalue() {
    let mut state = State::new();
    state.gc_disable_auto();

    state
        .load_string(
            r#"
        local shared = {value = 0}
        closures = {}

        for i = 1, 10 do
            closures[i] = function()
                shared.value = shared.value + 1
                return shared.value
            end
        end
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.gc_collect();

    // Call each closure, all should increment the same shared table
    for i in 1..=10 {
        state
            .load_string(format!("return closures[{i}]()"))
            .unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        assert_eq!(
            result, i as f64,
            "Shared upvalue should be incremented to {i}"
        );
        state.pop(1);

        // GC between calls
        state.gc_collect();
    }
}

fn run_number(source: &str) -> f64 {
    let mut state = State::new();
    state.load_string(source).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let result = state.to_number(-1).unwrap();
    state.pop(1);
    result
}

#[test]
fn block_upvalue_closes_before_slot_reuse() {
    assert_eq!(
        run_number("do local x = 1; f = function() return x end end; local x = 2; return f()"),
        1.0
    );
}

#[test]
fn if_arm_upvalue_closes_and_false_branch_reaches_next_arm() {
    assert_eq!(
        run_number(
            "if false then local x = 1; f = function() return x end elseif true then local x = 2; f = function() return x end else local x = 3; f = function() return x end end; local x = 4; return f()"
        ),
        2.0
    );
}

#[test]
fn numeric_for_closes_visible_variable_each_iteration() {
    assert_eq!(
        run_number(
            "t = {}; for i = 1, 3 do t[i] = function() return i end end; return t[1]() * 100 + t[2]() * 10 + t[3]()"
        ),
        123.0
    );
}

#[test]
fn generic_for_closes_all_visible_variables_each_iteration() {
    assert_eq!(
        run_number(
            "t = {}; for k, v in ipairs({10, 20, 30}) do t[k] = function() return k * 100 + v end end; return t[1]() + t[2]() + t[3]()"
        ),
        660.0
    );
}

#[test]
fn break_closes_nested_loop_local_before_slot_reuse() {
    assert_eq!(
        run_number(
            "while true do do local x = 7; f = function() return x end; break end end; local x = 8; return f()"
        ),
        7.0
    );
}

#[test]
fn while_iterations_keep_outer_upvalue_shared() {
    assert_eq!(
        run_number(
            "local counter = 0; while counter < 3 do counter = counter + 1; if counter == 1 then f = function() return counter end end end; return f() * 10 + counter"
        ),
        33.0
    );
}

// Plain `if ... then ... end` with no following arm: the taken branch closes
// its captured local via the level_down in close_if_arm's no-next-arm path.
#[test]
fn plain_if_arm_closes_captured_local_before_slot_reuse() {
    assert_eq!(
        run_number(
            "if true then local x = 1; f = function() return x end end; local x = 2; return f()"
        ),
        1.0
    );
}

// break capturing a direct while-body local (not nested in a do): break's
// pre-jump close handles it, and the loop's final level_down emits a second,
// harmless close at the same base.
#[test]
fn break_closes_direct_while_local_before_slot_reuse() {
    assert_eq!(
        run_number(
            "while true do local x = 5; f = function() return x end; break end; local x = 6; return f()"
        ),
        5.0
    );
}
