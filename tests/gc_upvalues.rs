//! Tests for GC handling of closed upvalues in closures.
//!
//! These tests verify that values captured in closures survive garbage collection
//! even after the original scope has ended and the upvalue has been "closed"
//! (moved from the stack to the upvalue pool).

use dellingr::{ArgCount, RetCount, State};

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
