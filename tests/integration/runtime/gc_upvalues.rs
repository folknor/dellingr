//! GC roots held by closures, executing callable values and active varargs.

use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn install_force_gc(state: &mut State) {
    state
        .push_new_rust_fn(|state| {
            state.gc_collect();
            Ok(0)
        })
        .unwrap();
    state.set_global("force_gc").unwrap();
}

fn install_force_next_gc(state: &mut State) {
    state
        .push_new_rust_fn(|state| {
            state.gc_set_threshold(1);
            Ok(0)
        })
        .unwrap();
    state.set_global("force_next_gc").unwrap();
}

#[test]
fn open_upvalue_survives_gc_while_defining_frame_is_active() {
    let mut state = State::new();
    state.gc_disable_auto();
    install_force_gc(&mut state);

    support::eval(
        &mut state,
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
        1,
    );

    assert_eq!(state.to_number(-1).unwrap(), 55.0);
    state.pop(1).unwrap();
}

#[test]
fn executing_returned_closure_roots_closed_upvalues_during_auto_gc() {
    let mut state = State::raw();

    support::load(
        &mut state,
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
    );
    state.gc_set_threshold(1);
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1).unwrap();
}

#[test]
fn temporary_callable_table_survives_gc_during_metamethod_handler() {
    let mut state = State::new();
    install_force_next_gc(&mut state);

    support::eval(
        &mut state,
        r#"
        local function make_callable()
            local callable = setmetatable({ value = 73 }, {
                __call = function(self)
                    local trigger_gc = {}
                    return self.value
                end
            })
            force_next_gc()
            return callable
        end

        return make_callable()()
    "#,
        1,
    );

    assert_eq!(state.to_number(-1).unwrap(), 73.0);
    state.pop(1).unwrap();
}

/// Basic test: closure captures a table, table survives GC.
#[test]
fn closure_captured_table_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto(); // Manual GC control

    // Create a closure that captures a local table
    support::run(
        &mut state,
        r#"
        local function make_closure()
            local captured = {value = 42}
            return function() return captured.value end
        end
        test_fn = make_closure()
    "#,
    );

    // Force GC - the captured table should survive
    state.gc_collect();

    // Call the closure - should still work
    state.get_global("test_fn").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 42.0, "Captured table value should survive GC");
    state.pop(1).unwrap();
}

/// Test multiple GC cycles with closure holding table.
#[test]
fn closure_survives_multiple_gc_cycles() {
    let mut state = State::new();
    state.gc_disable_auto();

    support::run(
        &mut state,
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
    );

    // Call and GC multiple times
    for expected in 1..=5 {
        state.gc_collect();

        state.get_global("counter").unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

        let result = state.to_number(-1).unwrap();
        assert_eq!(
            result, expected as f64,
            "Counter should be {expected} after GC cycle"
        );
        state.pop(1).unwrap();
    }
}

/// Test closure capturing another closure (nested upvalues).
#[test]
fn nested_closures_survive_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    support::run(
        &mut state,
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
    );

    state.gc_collect();

    state.get_global("nested_fn").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 30.0, "Nested closure should access both upvalues");
    state.pop(1).unwrap();
}

/// Test metatable stored as upvalue (the original bug case).
#[test]
fn metatable_as_upvalue_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    // This pattern mirrors fleet:group() - metatable defined once, used by closure
    support::run(
        &mut state,
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
    );

    // Force GC - mt should survive because create_object closure references it
    state.gc_collect();

    // Create an object using the closure
    state.get_global("create_object").unwrap();
    state.push_number(99.0).unwrap();
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    state.set_global("obj").unwrap();

    // Access via metatable method
    support::eval(&mut state, "return obj:get_value()", 1);

    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 99.0, "Metatable method should work after GC");
    state.pop(1).unwrap();
}

/// Test that unreferenced closures ARE collected.
#[test]
fn unreferenced_closure_is_collected() {
    let mut state = State::new();
    state.gc_disable_auto();

    let source = r#"
        local function make_and_discard()
            local big_table = {a=1, b=2, c=3, d=4}
            local fn = function() return big_table end
            -- fn goes out of scope here, not stored anywhere
        end
        make_and_discard()
    "#;

    // Establish the baseline after a collection has already reclaimed the
    // chunk closure from this script.
    support::run(&mut state, source);
    state.gc_collect();
    let baseline = state.object_count();

    // The script creates GC-managed objects, all of which should be reclaimed.
    support::run(&mut state, source);
    let before_gc = state.object_count();
    assert!(
        before_gc > baseline,
        "objects should be created by the script"
    );
    state.gc_collect();

    let after_gc = state.object_count();
    assert_eq!(
        before_gc - after_gc,
        before_gc - baseline,
        "GC should reclaim the exact object-count increase \
         (baseline={baseline}, before GC={before_gc}, after GC={after_gc})"
    );
}

/// Test closure with multiple upvalues of different types.
#[test]
fn closure_with_mixed_upvalues() {
    let mut state = State::new();
    state.gc_disable_auto();

    support::run(
        &mut state,
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
    );

    state.gc_collect();

    state.get_global("mixed_fn").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();

    assert_eq!(state.to_number(-4).unwrap(), 42.0);
    assert_eq!(state.to_string(-3).unwrap(), "hello");
    assert_eq!(state.to_number(-2).unwrap(), 1.0);
    assert_eq!(state.to_string(-1).unwrap(), "inner");
    state.pop(4).unwrap();
}

/// Test closure stored in a table survives GC.
#[test]
fn closure_in_table_survives_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    support::run(
        &mut state,
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
    );

    state.gc_collect();

    // Check "answer"
    state.get_global("get").unwrap();
    state.push_string("answer").expect("short test string fits");
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1).unwrap();

    state.gc_collect();

    // Check "pi"
    state.get_global("get").unwrap();
    state.push_string("pi").expect("short test string fits");
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    let pi = state.to_number(-1).unwrap();
    assert!((pi - std::f64::consts::PI).abs() < 0.0001);
    state.pop(1).unwrap();
}

/// Stress test: many closures with shared upvalue.
#[test]
fn many_closures_shared_upvalue() {
    let mut state = State::new();
    state.gc_disable_auto();

    support::run(
        &mut state,
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
    );

    state.gc_collect();

    // Call each closure, all should increment the same shared table
    for i in 1..=10 {
        support::eval(&mut state, format!("return closures[{i}]()"), 1);
        let result = state.to_number(-1).unwrap();
        assert_eq!(
            result, i as f64,
            "Shared upvalue should be incremented to {i}"
        );
        state.pop(1).unwrap();

        // GC between calls
        state.gc_collect();
    }
}

#[test]
fn nested_vararg_arena_roots_and_releases_each_activation() {
    let mut state = State::new();
    install_force_gc(&mut state);
    // Collect, then report how many tables and closures are still reachable.
    state
        .push_new_rust_fn(|state| {
            state.gc_collect();
            state.push_number(state.object_count() as f64)?;
            Ok(1)
        })
        .unwrap();
    state.set_global("live_objects").unwrap();
    // Each inner call's tables are reachable only through its arena suffix.
    // Once it returns, a collection must free them: the reachable count after
    // the call equals the count before it. If the arena kept a returned
    // activation's values, they would stay rooted and the count would grow.
    support::run(
        &mut state,
        r#"
            local function empty()
                force_gc()
            end
            local function inner(...)
                empty()
                force_gc()
                local value = ...
                if value.marker ~= 23 then error("inner vararg lost") end
            end
            local function outer(...)
                local before = live_objects()
                inner({ marker = 23 })
                if live_objects() ~= before then
                    error("returned single vararg stayed reachable")
                end
                inner({ marker = 23 }, {}, {})
                if live_objects() ~= before then
                    error("returned varargs stayed reachable")
                end
                empty()
                force_gc()
                local value, missing, text = ...
                if value.marker ~= 42 or missing ~= nil or text ~= "kept" then
                    error("outer varargs changed")
                end
            end
            outer({ marker = 42 }, nil, "kept")
            outer({ marker = 42 }, nil, "kept")
        "#,
    );
}
