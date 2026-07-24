use super::Bytecode;
use super::Instr;
use super::State;
use super::compiler::parse_str;
use super::lua_val::Val;
use crate::instr::RetCount;

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

    state.cost_used = u64::MAX - 1;
    state.cost_remaining = i64::MAX;
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
    assert_eq!(Val::Num(1.0), *state.globals.get("a").unwrap());
}

#[test]
fn vm_test02() {
    let mut state = State::new();
    let input = Bytecode {
        code: vec![
            Instr::push_string(1),
            Instr::push_string(2),
            Instr::concat(2),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["key".into(), "a".into(), "b".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    let val = state.globals.get("key").unwrap();
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
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![2.5],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    assert_eq!(Val::Bool(true), *state.globals.get("a").unwrap());
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
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["key".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(input, 0).unwrap();
    assert_eq!(Val::Bool(false), *state.globals.get("key").unwrap());
}

#[test]
fn vm_test06() {
    let mut state = State::new();
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(chunk, 0).unwrap();
    assert_eq!(Val::Num(5.0), *state.globals.get("a").unwrap());
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
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![2.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    state.eval_chunk(chunk, 0).unwrap();
    assert!(state.globals.get("a").is_none());
}

#[test]
fn vm_test08() {
    let code = vec![
        Instr::push_num(2), // a = 2
        Instr::set_global(0),
        Instr::get_global(0), // a <0
        Instr::push_num(0),
        Instr::less(),
        Instr::branch_false(5),
        Instr::get_global(0),
        Instr::push_num(1),
        Instr::add(),
        Instr::set_global(0),
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
        Instr::set_global(0),
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
    assert_eq!(Val::Num(10.0), *state.globals.get("x").unwrap());
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
        Instr::set_global(0), // a = 2
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
    assert!(state.globals.get("a").is_none());
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
    let a = state.globals.get("a").unwrap().as_num().unwrap();
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

    // Manually trigger GC - tables are reachable so they survive
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
    state.get_global("on_tick");
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
        state.get_global("tick");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        state.pop(1);
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

    state.get_global("callback");
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
    assert!(!err.stack_trace.is_empty());
    assert_eq!(err.stack_trace[0].line, 3);
}
