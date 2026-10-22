use super::Bytecode;
use super::Instr;
use super::State;
use super::compiler::parse_str;
use super::lua_val::Val;
use crate::instr::RetCount;

fn force_gc(state: &mut State) -> crate::Result<u8> {
    state.gc_collect();
    Ok(0)
}

#[test]
fn auto_gc_marks_a_deep_lua_table_chain_iteratively() {
    let mut state = State::new();
    state
        .load_string("t = {}\nfor i = 1, 100000 do t = { t } end\ndone = t ~= nil")
        .expect("deep chain compiles");
    state
        .call(crate::instr::ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("deep chain completes without overflowing the native stack");
}

#[test]
fn consume_cost_saturates_large_host_charges() {
    for (cost, remaining) in [
        (i64::MAX as u64, 0),
        (i64::MAX as u64 + 1, -1),
        (u64::MAX, i64::MIN),
    ] {
        let mut state = State::new();
        state.set_cost_budget(i64::MAX);
        state
            .consume_cost(cost)
            .expect("first charge may cross the budget");
        assert_eq!(state.cost_remaining(), remaining);
        assert_eq!(state.cost_used(), cost);
    }

    let mut state = State::new();
    state.set_cost_budget(1);
    state
        .consume_cost(u64::MAX)
        .expect("first charge may cross the budget");
    assert!(state.consume_cost(1).is_err());

    // Reach a near-saturated meter through the API. Count-only, so the
    // second charge cannot be blocked by an exhausted budget.
    let mut state = State::new();
    state
        .consume_cost(u64::MAX - 1)
        .expect("count-only charges are never blocked");
    state
        .consume_cost(2)
        .expect("saturating used-cost charge should succeed");
    assert_eq!(state.cost_used(), u64::MAX);
}

#[test]
fn vm_test01() {
    let mut state = State::new();
    let input = parse_str("a = 1").unwrap();
    state.eval_chunk(input, 0).unwrap();
    assert_eq!(Val::Num(1.0), *state.global_env.get("a").unwrap());
}

#[test]
fn nil_assignment_does_not_create_unknown_globals_including_builtins() {
    let mut state = State::empty();
    state.load_string("never_set = nil; print = nil").unwrap();
    state
        .call(crate::instr::ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap();
    assert!(state.global_env.is_empty());
    for name in ["never_set", "print"] {
        state.push_nil().unwrap();
        state.set_global(name).unwrap();
    }
    assert!(state.global_env.is_empty());
}

fn run_lua(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state
        .call(crate::instr::ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap();
}

const COMPUTED_NAME_CHURN: &str = "
    for i = 1, 2000 do
        _G['k' .. i] = i
        _G['k' .. i] = nil
    end
    for i = 1, 2000 do
        local name = 'c' .. i
        c_cached = i
        c_cached = nil
        _G[name] = true
        _G[name] = nil
    end
";

#[test]
fn sealed_globals_compact_computed_name_churn() {
    let mut state = State::new();
    run_lua(&mut state, "setup_value = 1; setup_value = nil");
    state.seal_globals();
    let baseline = state.global_env.len();
    run_lua(&mut state, COMPUTED_NAME_CHURN);
    // The map stays within twice its live-plus-pinned size, whatever the
    // number of names the script has used.
    assert!(state.global_env.len() <= 2 * baseline + 2);
    // A tombstone written before sealing survives every compaction.
    assert_eq!(state.global_env.get("setup_value"), Some(&Val::Nil));
}

#[test]
fn unsealed_globals_keep_every_tombstone() {
    let mut state = State::new();
    let baseline = state.global_env.len();
    run_lua(&mut state, COMPUTED_NAME_CHURN);
    assert_eq!(state.global_env.len(), baseline + 4001);
}

#[test]
fn compaction_keeps_live_order_and_cached_writes() {
    let mut state = State::new();
    state.seal_globals();
    let baseline = state.global_env.len();
    // `bump` warms its GET_GLOBAL/SET_GLOBAL caches on `kept`, compaction then
    // moves `kept` down by the five `t` tombstones before it, and the second
    // call reuses the same instructions. The stale index stays in bounds and
    // now names `u5`, so only the version bump keeps the write off it.
    run_lua(
        &mut state,
        "
        function bump() kept = kept + 1 end
        for i = 1, 5 do _G['t' .. i] = i end
        kept = 0
        for i = 1, 10 do _G['u' .. i] = i end
        for i = 1, 200 do _G['v' .. i] = i end
        bump()
        for i = 1, 5 do _G['t' .. i] = nil end
        for i = 1, 200 do _G['v' .. i] = nil end
        last = 1
        bump()
        ",
    );
    let names: Vec<&str> = state
        .global_env
        .iter()
        .map(|(name, _)| name.as_str())
        .skip(baseline)
        .collect();
    let mut expected = vec!["bump".to_string(), "kept".to_string()];
    expected.extend((1..=10).map(|i| format!("u{i}")));
    expected.push("last".to_string());
    assert_eq!(names, expected);
    assert_eq!(state.global_env.get("kept"), Some(&Val::Num(2.0)));
    assert_eq!(state.global_env.get("u5"), Some(&Val::Num(5.0)));
}

#[test]
fn restricted_env_counts_a_repeated_whitelist_name_once() {
    let mut state = State::new();
    state.seal_globals();
    run_lua(&mut state, "x = 1; x = nil");
    state
        .with_restricted_env(&["x", "x"], |state| {
            // Collection checks the tombstone count in debug builds.
            state.gc_collect();
        })
        .unwrap();
}

#[test]
fn vm_test02() {
    let mut state = State::new();
    let input = Bytecode {
        code: vec![
            Instr::push_string(1),
            Instr::push_string(2),
            Instr::concat(2),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["key".into(), "a".into(), "b".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    let val = state.global_env.get("key").unwrap();
    assert_eq!(val.as_string(&state.heap), Some(&b"ab"[..]));
}

#[test]
fn vm_test04() {
    let mut state = State::new();
    let input = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(0),
            Instr::equal(),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![2.5],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    assert_eq!(Val::Bool(true), *state.global_env.get("a").unwrap());
}

#[test]
fn vm_test05() {
    let mut state = State::new();
    let input = Bytecode {
        code: vec![
            Instr::push_bool(true),
            Instr::branch_false_keep(2),
            Instr::pop(),
            Instr::push_bool(false),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["key".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    assert_eq!(Val::Bool(false), *state.global_env.get("key").unwrap());
}

#[test]
fn vm_test06() {
    let mut state = State::new();
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(chunk, 0).unwrap();
    assert_eq!(Val::Num(5.0), *state.global_env.get("a").unwrap());
}

#[test]
fn vm_test07() {
    let mut state = State::new();
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(0),
        Instr::less(),
        Instr::branch_false(2),
        Instr::push_bool(true),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![2.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(chunk, 0).unwrap();
    assert!(state.global_env.get("a").is_none());
}

#[test]
fn vm_test08() {
    let code = vec![
        Instr::push_num(2), // a = 2
        Instr::set_global(0, None),
        Instr::get_global(0, None), // a <0
        Instr::push_num(0),
        Instr::less(),
        Instr::branch_false(5),
        Instr::get_global(0, None),
        Instr::push_num(1),
        Instr::add(),
        Instr::set_global(0, None),
        Instr::jump(-9),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 10.0, 0.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    let mut state = State::new();
    state.eval_chunk(chunk, 0).unwrap();
}

#[test]
fn vm_test09() {
    // local a = 1
    // while a < 10 do
    //   a = a + 1
    // end
    // x = a
    let code = vec![
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::get_local(0),
        Instr::push_num(1),
        Instr::less(),
        Instr::branch_false(5),
        Instr::get_local(0),
        Instr::push_num(2),
        Instr::add(),
        Instr::set_local(0),
        Instr::jump(-9),
        Instr::get_local(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 10.0, 1.0],
        string_literals: vec!["x".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    let mut state = State::new();
    state.eval_chunk(chunk, 0).unwrap();
    assert_eq!(Val::Num(10.0), *state.global_env.get("x").unwrap());
}

#[test]
fn vm_test10() {
    let code = vec![
        // For loop control variables
        Instr::push_num(0), // start = 6
        Instr::push_num(1), // limit = 2
        Instr::push_num(1), // step = 2
        // Start loop
        Instr::for_prep(0, 3),
        Instr::push_num(0),
        Instr::set_global(0, None), // a = 2
        // End loop
        Instr::for_loop(0, -3),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![6.0, 2.0],
        string_literals: vec!["a".into()],
        num_locals: 4,
        ..Bytecode::default()
    };
    let mut state = State::new();
    state.eval_chunk(chunk, 0).unwrap();
    assert!(state.global_env.get("a").is_none());
}

#[test]
fn vm_test11() {
    let text = "
        a = 0
        for i = 1, 3 do
            a = a + i
        end";
    let chunk = parse_str(text).unwrap();
    let mut state = State::new();
    state.eval_chunk(chunk, 0).unwrap();
    let a = state.global_env.get("a").unwrap().as_num().unwrap();
    assert_eq!(a, 6.0);
}

#[test]
fn gc_host_controlled() {
    let mut state = State::new();

    // Check initial state
    let initial_objects = state.object_count();
    let initial_strings = state.string_count();
    assert!(state.heap_size() >= initial_objects + initial_strings);

    // Disable auto-GC
    state.gc_disable_auto();
    assert_eq!(state.gc_threshold(), usize::MAX);

    // Create some tables - GC won't trigger automatically
    let code = parse_str("t1 = {} t2 = {} t3 = {}").unwrap();
    state.eval_chunk(code, 0).unwrap();

    // Should have more objects now
    assert!(state.object_count() > initial_objects);

    // Manually trigger GC - tables are reachable so they survive. The first
    // collection frees the chunk's own closure, which `eval_chunk` places in
    // the call's function slot like any call and is garbage once it returns.
    state.gc_collect();
    let before_gc = state.object_count();
    state.gc_collect();
    assert_eq!(state.object_count(), before_gc); // All tables are reachable

    // Remove references and collect
    let code = parse_str("t1 = nil t2 = nil t3 = nil").unwrap();
    state.eval_chunk(code, 0).unwrap();
    state.gc_collect();

    // Now the tables should be collected
    assert!(state.object_count() < before_gc);
}

#[test]
fn gc_collect_preserves_disabled_auto_gc() {
    let mut state = State::new();
    state.gc_disable_auto();

    let code = parse_str("t1 = {} t2 = {} t3 = {}").unwrap();
    state.eval_chunk(code, 0).unwrap();

    // An explicit collection must not clobber the disabled-auto sentinel.
    state.gc_collect();
    assert_eq!(state.gc_threshold(), usize::MAX);
    assert!(!state.gc_should_run());

    // Resuming the automatic policy restores adaptive recomputation.
    state.gc_set_auto_threshold();
    state.gc_collect();
    assert_ne!(state.gc_threshold(), usize::MAX);
    assert!(state.gc_threshold() >= 20);
}

#[test]
fn gc_preserves_nested_suspended_environments() {
    let mut state = State::new();
    state.new_table().unwrap();
    let original = state.pop_val();
    state.set_global_value("original", original);

    state
        .with_restricted_env(&[], |state| {
            state.new_table().unwrap();
            let outer = state.pop_val();
            state.set_global_value("outer", outer);

            state
                .with_restricted_env(&[], |state| {
                    state.gc_collect();
                })
                .expect("state is not poisoned");

            let outer = *state
                .global_env
                .get("outer")
                .expect("outer restricted value must be restored");
            // Dereference through the heap. `as_object_ptr().is_some()` only
            // inspects the Val variant, so a swept generational pointer still
            // returns Some and the assertion could not fail.
            assert!(
                live_table(state, outer),
                "outer restricted value was collected"
            );
        })
        .expect("state is not poisoned");

    let original = *state
        .global_env
        .get("original")
        .expect("original value must be restored");
    assert!(
        live_table(&state, original),
        "original global was collected"
    );
    let math = state.global_env.builtin(crate::instr::Builtin::Math as u8);
    assert!(live_table(&state, math), "math library table was collected");
}

/// True only if `val` is a table that is still resolvable on the heap. Used by
/// the GC-root tests, where checking the `Val` variant alone would pass against
/// a dangling pointer.
fn live_table(state: &State, val: Val) -> bool {
    val.as_object_ptr()
        .and_then(|ptr| state.heap.as_table_ref(ptr))
        .is_some()
}

#[test]
fn gc_preserves_frame_varargs() {
    let mut state = State::new();
    state.set_global_value("force_gc", Val::RustFn(force_gc));
    let input = parse_str(
        r#"
        local function f(...)
            force_gc()
            return ...
        end
        return f({ marker = 42 }).marker
        "#,
    )
    .expect("vararg script must parse");
    state
        .eval_chunk(input, 0)
        .expect("vararg object must survive GC");
    assert_eq!(state.to_number(-1).expect("return must be numeric"), 42.0);
}

#[test]
fn table_sort_array_survives_comparator_collection() {
    let mut state = State::new();
    state.set_global_value("force_gc", Val::RustFn(force_gc));
    let input = parse_str(
        r#"
        local t = {}
        for i = 1, 20 do t[i] = { v = 21 - i } end
        local collected = false
        table.sort(t, function(a, b)
            if not collected then
                collected = true
                for k in pairs(t) do t[k] = nil end
                force_gc()
            end
            return a.v < b.v
        end)
        return t[1].v, t[20].v
        "#,
    )
    .expect("sort script must parse");
    state
        .eval_chunk(input, 0)
        .expect("detached sort array must survive comparator GC");
    assert_eq!(
        state.to_number(-2).expect("first result must be numeric"),
        1.0
    );
    assert_eq!(
        state.to_number(-1).expect("last result must be numeric"),
        20.0
    );
}

#[test]
fn set_table_str_key_value_roots_heap_value() {
    let mut state = State::empty();
    state.gc_disable_auto();
    let child = state.alloc_table();
    state.new_table().unwrap();
    state.gc_set_threshold(state.heap_size());

    state
        .set_table_str_key_value(1, "child", child)
        .expect("setting rooted heap value must succeed");
    state.push_string("child").expect("short test string fits");
    state.get_table_raw(1).expect("child lookup must succeed");
    // Must dereference: if interning the key had collected the child, the
    // stale pointer would still satisfy an `as_object_ptr().is_some()` check.
    let stored = state.pop_val();
    assert!(
        live_table(&state, stored),
        "child was collected while interning the key"
    );
}

#[test]
fn gc_threshold_control() {
    let mut state = State::empty(); // Empty state, no stdlib tables

    // Set a custom threshold
    state.gc_set_threshold(100);
    assert_eq!(state.gc_threshold(), 100);

    // Should not need GC yet
    assert!(!state.gc_should_run());

    // Set very low threshold
    state.gc_set_threshold(1);

    // Create a table to exceed threshold
    let code = parse_str("t = {}").unwrap();
    state.eval_chunk(code, 0).unwrap();

    // Now GC should be needed (but won't auto-run since we're just checking)
    // Note: threshold may have been adjusted by auto-GC during eval
}

#[test]
fn string_allocations_drive_automatic_gc_threshold() {
    let mut state = State::empty();
    // An automatic policy, not a fixed host threshold, so collection adapts.
    state.gc_set_policy(20, 20, 2);

    // Keep enough distinct strings rooted to reach the threshold. The State's
    // interned metamethod names are live strings from construction on.
    for i in 0..20 - super::MetaName::ALL.len() {
        state
            .push_string(format!("live-{i}"))
            .expect("short test string fits");
    }

    assert_eq!(state.object_count(), 0);
    assert_eq!(state.string_count(), 20);
    assert!(state.gc_should_run());

    // This allocation must collect first. All 20 strings survive, so the
    // adaptive threshold must include them and grow to 40.
    state
        .push_string("trigger")
        .expect("short test string fits");

    assert_eq!(state.string_count(), 21);
    assert_eq!(state.gc_threshold(), 40);
    assert!(!state.gc_should_run());

    // Drop every live string, then verify string-only churn is collected
    // automatically rather than growing without bound.
    state.set_top(0).unwrap();
    for i in 0..1_000 {
        state
            .push_string(format!("temporary-{i}"))
            .expect("short test string fits");
        state.pop(1).unwrap();
    }

    assert_eq!(state.object_count(), 0);
    assert!(
        state.string_count() <= state.gc_threshold(),
        "temporary strings should have triggered automatic collection"
    );
}

/// Test the callback pattern used by fcomm2:
/// - Main chunk defines local functions and global callbacks that capture them
/// - Main chunk finishes (upvalues should be closed)
/// - Later, the global callback is called from Rust (simulating game tick)
#[test]
fn callback_pattern_local_upvalue() {
    use crate::{ArgCount, RetCount};

    let mut state = State::new();

    // Load and execute main chunk that defines a global callback
    // capturing a local function
    let code = r#"
        local function helper()
            return 42
        end

        function on_tick()
            return helper()
        end
    "#;

    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Now the main chunk has finished. The upvalue for `helper` should be closed.
    // Call the global callback from "outside" (simulating fcomm2's callback pattern)
    state.get_global("on_tick").unwrap();
    assert_eq!(state.typ(-1), crate::LuaType::Function);

    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    // Should get 42 back
    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 42.0);
}

/// More complex callback pattern with mutable upvalue
#[test]
fn callback_pattern_mutable_upvalue() {
    use crate::{ArgCount, RetCount};

    let mut state = State::new();

    let code = r#"
        local counter = 0

        local function increment()
            counter = counter + 1
            return counter
        end

        function tick()
            return increment()
        end
    "#;

    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Call tick multiple times
    for expected in 1..=5 {
        state.get_global("tick").unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        state.pop(1).unwrap();
        assert_eq!(result, expected as f64);
    }
}

/// Nested local functions with upvalues
#[test]
fn callback_pattern_nested_locals() {
    use crate::{ArgCount, RetCount};

    let mut state = State::new();

    let code = r#"
        local base = 100

        local function inner()
            return base
        end

        local function outer()
            return inner() + 10
        end

        function callback()
            return outer() + 1
        end
    "#;

    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.get_global("callback").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let result = state.to_number(-1).unwrap();
    assert_eq!(result, 111.0); // 100 + 10 + 1
}

/// Test that error line numbers are accurate
#[test]
fn error_line_numbers() {
    use crate::{ArgCount, RetCount};

    let mut state = State::new();

    // Error is on line 3 (t() call), not line 2 (t = {})
    let code = "-- comment\nlocal t = {}\nt()";

    state.load_string(code).unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

    assert!(result.is_err());
    let err = result.unwrap_err();
    // Check the stack trace points to line 3
    assert!(!err.stack_trace().is_empty());
    assert_eq!(err.stack_trace()[0].line, 3);
}

// --- MAX_STACK_SIZE is a real cap on the shared Lua/Rust value stack ---

use super::MAX_STACK_SIZE;

/// Fill the value stack to exactly one slot below the cap.
fn fill_to_one_below_cap(state: &mut State) {
    state
        .set_top((MAX_STACK_SIZE - 1) as isize)
        .expect("filling to one below the cap must be allowed");
    assert_eq!(state.get_top(), MAX_STACK_SIZE - 1);
}

#[test]
fn last_slot_below_the_cap_is_usable() {
    let mut state = State::empty();
    fill_to_one_below_cap(&mut state);

    // The cap is exclusive of nothing: the millionth value is still legal.
    state.push_nil().expect("the final slot must be usable");
    assert_eq!(state.get_top(), MAX_STACK_SIZE);
}

#[test]
fn every_push_type_is_rejected_at_the_cap() {
    let mut state = State::empty();
    fill_to_one_below_cap(&mut state);
    state.push_nil().expect("the final slot must be usable");

    let top = state.get_top();
    assert_eq!(top, MAX_STACK_SIZE);

    // Each public push must refuse, and must leave the stack untouched.
    assert!(state.push_nil().is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_number(1.0).is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_boolean(true).is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_rust_fn(force_gc).is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_string("x").is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_bytes(b"x").is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.push_value(1).is_err());
    assert_eq!(state.get_top(), top);
    assert!(state.new_table().is_err());
    assert_eq!(state.get_top(), top);
}

#[test]
fn a_rejected_push_reports_stack_overflow() {
    let mut state = State::empty();
    fill_to_one_below_cap(&mut state);
    state.push_nil().expect("the final slot must be usable");

    let err = state.push_nil().expect_err("the cap must be enforced");
    assert!(
        matches!(err.kind(), crate::error::ErrorKind::StackOverflow { .. }),
        "expected StackOverflow, got {:?}",
        err.kind()
    );
}

#[test]
fn set_top_cannot_exceed_the_cap() {
    let mut state = State::empty();
    // The bulk path preflights, so it must refuse in one shot rather than
    // allocating most of the way and then failing.
    assert!(state.set_top((MAX_STACK_SIZE + 1) as isize).is_err());
    assert_eq!(state.get_top(), 0);
}

#[test]
fn the_cap_does_not_charge_cost() {
    let mut state = State::empty();
    fill_to_one_below_cap(&mut state);
    state.push_nil().expect("the final slot must be usable");

    let before = state.cost_used();
    assert!(state.push_nil().is_err());
    assert_eq!(
        state.cost_used(),
        before,
        "enforcing the stack cap must not charge the cost budget"
    );
}

#[test]
fn table_next_preflights_its_net_growth_before_replacing_the_key() {
    // `next` replaces one key with two values, so it needs one free slot
    // measured against the entry stack, before the key is consumed.
    let mut state = State::empty();
    state.new_table().expect("room for the table");
    state.push_string("k").expect("room for the key");
    state.push_number(1.0).expect("room for the value");
    state.set_table_raw(1).expect("populating the table");

    // Top of stack is the traversal key that `table_next` consumes.
    state
        .set_top(MAX_STACK_SIZE as isize)
        .expect("filling to the cap");

    let err = state
        .table_next(1)
        .expect_err("popping one and pushing two must not fit at the cap");
    assert!(
        matches!(err.kind(), crate::error::ErrorKind::StackOverflow { .. }),
        "expected StackOverflow, got {:?}",
        err.kind()
    );
    assert_eq!(state.get_top(), MAX_STACK_SIZE);
    assert_eq!(state.typ(-1), crate::LuaType::Nil);

    state.pop(1).expect("shrinking frees the result slot");
    assert!(state.table_next(1).expect("one free slot is enough"));
    assert_eq!(state.get_top(), MAX_STACK_SIZE);
    assert_eq!(state.to_string(-2).unwrap(), "k");
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}

#[test]
fn table_remove_at_does_not_mutate_when_the_result_has_nowhere_to_go() {
    // The removed value can only be returned by pushing it. If the push is
    // going to fail, the element must still be in the table afterwards.
    let mut state = State::empty();
    state.new_table().expect("room for the table");
    state.push_number(1.0).expect("room for the key");
    state.push_number(42.0).expect("room for the value");
    state.set_table_raw(1).expect("populating the array part");
    assert_eq!(state.table_len(1).expect("index 1 holds the table"), 1);

    state
        .set_top(MAX_STACK_SIZE as isize)
        .expect("filling to the cap");

    assert!(
        state.table_remove_at(1, 1).is_err(),
        "there is no slot for the removed value"
    );
    assert_eq!(
        state.table_len(1).expect("index 1 holds the table"),
        1,
        "the element must survive a rejected removal"
    );
}

#[test]
fn open_libs_is_all_or_nothing_against_the_cap() {
    // `State::open_libs` is public, so a host can call it on a full stack. It
    // must refuse up front rather than installing part of the standard library.
    let mut state = State::empty();
    assert!(
        state.global_env.is_empty(),
        "empty() starts with no globals"
    );

    // Three free slots allow early global installs but not the four values
    // needed for `_G`'s protection field. Without the up-front reservation,
    // a per-push check would fail after globals were already installed.
    state
        .set_top((MAX_STACK_SIZE - 3) as isize)
        .expect("filling to just below the cap");

    assert!(state.open_libs().is_err(), "no headroom for library setup");
    assert!(
        state.global_env.is_empty(),
        "a rejected open_libs must not install any globals"
    );
}

#[test]
fn open_libs_succeeds_with_exactly_four_slots_of_headroom() {
    let mut state = State::empty();
    state.set_top((MAX_STACK_SIZE - 4) as isize).unwrap();
    state.open_libs().expect("four slots cover the full setup");
    assert_eq!(state.get_top(), MAX_STACK_SIZE - 4);
    state.get_global("_G").unwrap();
    state.get_metatable_of(-1).unwrap();
    state.push_string("__metatable").unwrap();
    state.get_table_raw(-2).unwrap();
    assert_eq!(state.stack.last(), Some(&Val::Bool(false)));
}

#[test]
fn open_libs_with_values_on_the_stack_targets_its_own_g_table() {
    // `_G` setup must address its table relative to the top, not absolute
    // index 1, or a host value already on the stack receives the metatable.
    let mut state = State::empty();
    state.new_table().expect("host table fits");
    state
        .open_libs()
        .expect("stdlib installs above a host value");
    assert_eq!(state.get_top(), 1, "open_libs leaves the host value alone");
    state.get_metatable_of(1).expect("index 1 is valid");
    assert_eq!(
        state.typ(-1),
        crate::LuaType::Nil,
        "the host table must not receive _G's metatable"
    );
    state.pop(1).expect("metatable slot");

    state.get_global("_G").expect("_G fits");
    state.get_metatable_of(-1).expect("_G is on the stack");
    assert_eq!(
        state.typ(-1),
        crate::LuaType::Table,
        "_G keeps its proxy metatable"
    );
}

/// A rejected `push_named_rust_fn` must not leave the id registered. That is
/// not directly observable, but it is visible through a save: an unregistered
/// reachable `RustFunc` fails the save, so if the rejected push had registered
/// the id anyway, the save below would succeed instead.
#[cfg(feature = "snapshot")]
#[test]
fn a_rejected_named_push_does_not_register_the_function() {
    fn host_fn(_state: &mut State) -> crate::Result<u8> {
        Ok(0)
    }

    let mut state = State::new();
    state
        .set_top(MAX_STACK_SIZE as isize)
        .expect("filling to the cap");
    assert!(
        state.push_named_rust_fn("host.fn", host_fn).is_err(),
        "the named push must be rejected at the cap"
    );

    // Back below the cap, reach the same function without an id.
    state.set_top(0).expect("shrinking is always allowed");
    state
        .push_rust_fn(host_fn)
        .expect("room again below the cap");
    state.set_global("host_fn").unwrap();

    let err = state
        .save_state()
        .expect_err("the id must not have been registered by the rejected push");
    assert!(
        matches!(
            err,
            crate::vm::save_state::SaveError::UnregisteredFunction { .. }
        ),
        "expected UnregisteredFunction, got {err:?}"
    );
}

#[test]
fn a_state_stays_usable_after_a_rejected_push() {
    let mut state = State::empty();
    fill_to_one_below_cap(&mut state);
    state.push_nil().expect("the final slot must be usable");
    assert!(state.push_nil().is_err());

    // Drop back well below the cap; the State must behave normally afterwards.
    state.set_top(2).expect("shrinking is always allowed");
    assert_eq!(state.get_top(), 2);
    state.push_number(7.0).expect("room again below the cap");
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn gc_sweeps_dead_upvalue_pool_slots_and_reuses_them() {
    let mut state = State::new();
    let make_counters = r#"
        counters = {}
        for i = 1, 100 do
            local n = i
            counters[i] = function() n = n + 1 return n end
        end
    "#;
    state
        .eval_chunk(parse_str(make_counters).unwrap(), 0)
        .expect("closure factory runs");
    state.gc_collect();
    let occupied_live = state.upvalue_pool.occupied_count();
    assert!(occupied_live >= 100, "each counter holds one captured slot");

    state
        .eval_chunk(parse_str("counters = nil").unwrap(), 0)
        .expect("release runs");
    state.gc_collect();
    assert!(
        state.upvalue_pool.occupied_count() < occupied_live - 90,
        "dead capture slots must be swept onto the free list"
    );

    // A second wave must reuse freed slots instead of growing the pool.
    let total_before = state.upvalue_pool.total_slots();
    state
        .eval_chunk(parse_str(make_counters).unwrap(), 0)
        .expect("second closure factory runs");
    assert_eq!(
        state.upvalue_pool.total_slots(),
        total_before,
        "the second wave should be served from the free list"
    );
}

#[test]
fn live_upvalues_survive_the_pool_sweep_and_keep_working() {
    let mut state = State::new();
    state.set_global_value("force_gc", Val::RustFn(force_gc));
    let input = parse_str(
        r#"
        local n = 10
        local function bump() n = n + 1 return n end
        bump()
        force_gc()
        return bump()
        "#,
    )
    .expect("upvalue script parses");
    state
        .eval_chunk(input, 0)
        .expect("live upvalue must survive the pool sweep");
    assert_eq!(state.to_number(-1).expect("numeric return"), 12.0);
}

#[test]
fn length_metamethod_preflights_three_slots_at_the_stack_cap() {
    fn len_handler(state: &mut State) -> crate::Result<u8> {
        assert_eq!(state.get_top(), 2);
        assert!(state.raw_equal(1, 2));
        *state
            .user_data_mut::<usize>()
            .expect("call counter is installed") += 1;
        state.push_number(7.0)?;
        Ok(1)
    }

    let mut state = State::new();
    state.set_user_data(0usize);
    state.set_global_value("len_handler", Val::RustFn(len_handler));
    state
        .load_string("t = setmetatable({}, { __len = len_handler })")
        .unwrap();
    state
        .call(crate::ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap();
    state.get_global("t").unwrap();
    for entry_size in [MAX_STACK_SIZE - 1, MAX_STACK_SIZE] {
        state.set_top((entry_size - 1) as isize).unwrap();
        state.push_value(1).unwrap();
        let error = state
            .instr_length(&mut 0)
            .expect_err("three slots do not fit");
        assert!(matches!(
            error.kind(),
            crate::error::ErrorKind::StackOverflow { size, limit }
                if *size == entry_size + 2 && *limit == MAX_STACK_SIZE
        ));
        assert_eq!(state.get_top(), entry_size - 1);
        assert_eq!(*state.user_data::<usize>().unwrap(), 0);
    }

    state.set_top((MAX_STACK_SIZE - 3) as isize).unwrap();
    state.push_value(1).unwrap();
    state
        .instr_length(&mut 0)
        .expect("three free slots after popping the operand");
    assert_eq!(state.get_top(), MAX_STACK_SIZE - 2);
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
    assert_eq!(*state.user_data::<usize>().unwrap(), 1);
}

#[test]
fn exhausted_format_pointer_counter_does_not_wrap_or_mutate() {
    let mut state = State::empty();
    state.new_table().unwrap();
    state.new_table().unwrap();
    state.next_format_pointer_id = u64::MAX - 1;
    assert_eq!(state.format_pointer_id(1).unwrap(), Some(u64::MAX - 1));
    assert_eq!(state.next_format_pointer_id, u64::MAX);

    let error = state
        .format_pointer_id(2)
        .expect_err("counter must not wrap");
    assert!(matches!(
        error.kind(),
        crate::error::ErrorKind::RuntimeError(message)
            if message == "format pointer identity space exhausted"
    ));
    assert!(matches!(
        error.library_error(),
        Some(crate::error::LibraryError::FormatPointerIdentitySpaceExhausted)
    ));
    assert_eq!(state.next_format_pointer_id, u64::MAX);
    assert_eq!(state.format_pointer_ids.len(), 1);
    assert_eq!(state.format_pointer_id(1).unwrap(), Some(u64::MAX - 1));
    state.push_number(f64::NAN).unwrap();
    assert_eq!(
        state.format_pointer_id(-1).unwrap(),
        None,
        "values without pointer identity never enter the id map"
    );
    assert_eq!(state.format_pointer_ids.len(), 1);
    state.pop_val();
    assert_eq!(state.get_top(), 2);
}

#[test]
fn gc_drops_format_pointer_ids_for_dead_values() {
    let mut state = State::new();
    // Auto-GC would sweep dead entries mid-loop; disable it so the assertion
    // below observes the accumulation and one explicit collect does the sweep.
    state.gc_disable_auto();
    let input = parse_str(
        r#"
        keep = {}
        local ids = string.format("%p", keep)
        for i = 1, 50 do
            ids = string.format("%p", { i })
        end
        "#,
    )
    .expect("%p script parses");
    state.eval_chunk(input, 0).expect("%p script runs");
    assert!(state.format_pointer_ids.len() >= 50);
    let next_id = state.next_format_pointer_id;
    state.gc_collect();
    assert!(
        state.format_pointer_ids.len() <= 2,
        "entries for collected values must be swept, got {}",
        state.format_pointer_ids.len()
    );
    // The id counter never rewinds: a swept identity is never reissued.
    assert_eq!(state.next_format_pointer_id, next_id);
    // The surviving global keeps its entry.
    assert!(
        state
            .format_pointer_ids
            .iter()
            .any(|(key, _)| live_table(&state, key.as_val())),
        "the reachable value's identity entry must survive"
    );
}
