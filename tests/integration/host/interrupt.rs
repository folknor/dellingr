//! `Interrupt`: a host request that stops running Lua at the next safepoint.
//! Each safepoint kind is driven deterministically by `stop()`, a host
//! function that triggers the running State's own handle, and the counters a
//! script leaves behind show the poll fired where it should: at the next
//! backward jump, activation start, or library-loop iteration, and not
//! later. One test triggers from another thread while a free loop spins.

use std::time::Duration;

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, Interrupt, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn stop(state: &mut State) -> dellingr::Result<u8> {
    state.interrupt().trigger();
    Ok(0)
}

/// Calls its function argument, discards whatever error it raised, and
/// returns normally.
fn swallow(state: &mut State) -> dellingr::Result<u8> {
    state.push_value(1)?;
    let _discarded = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    state.set_top(0)?;
    Ok(0)
}

fn install(state: &mut State) {
    let functions: [(&str, dellingr::RustFunc); 2] = [("stop", stop), ("swallow", swallow)];
    for (name, function) in functions {
        state
            .push_named_rust_fn(name, function)
            .expect("function fits");
        state.set_global(name).expect("global is set");
    }
}

fn state() -> State {
    let mut state = State::new();
    install(&mut state);
    state
}

fn assert_interrupted(result: dellingr::Result<()>, source: &str) -> Error {
    match result {
        Ok(()) => panic!("{source}\nmust be interrupted, but returned"),
        Err(error) => {
            assert!(
                matches!(error.kind(), ErrorKind::Interrupted),
                "{source}\nmust be interrupted, got {error}"
            );
            error
        }
    }
}

/// Runs `source` on a fresh State, which must end interrupted, and returns
/// the State for the caller to read back what the script left.
fn interrupted(source: &str) -> State {
    let mut state = state();
    assert_interrupted(support::try_run(&mut state, source), source);
    state
}

fn global_is_nil(state: &mut State, name: &str) -> bool {
    state.get_global(name).expect("the global is readable");
    let nil = state.typ(-1) == dellingr::LuaType::Nil;
    state.pop(1).expect("the global was pushed");
    nil
}

#[test]
fn a_free_loop_stops_when_another_thread_triggers() {
    let mut state = state();
    let interrupt = state.interrupt();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        interrupt.trigger();
    });
    let source = "while true do end";
    assert_interrupted(support::try_run(&mut state, source), source);
    trigger.join().expect("the trigger thread finishes");
}

#[test]
fn every_loop_shape_stops_at_its_next_back_edge() {
    for source in [
        "n = 0 while true do n = n + 1 if n == 3 then stop() end end",
        "n = 0 repeat n = n + 1 if n == 3 then stop() end until false",
        "n = 0 repeat n = n + 1 if n == 3 then stop() end until n > 9 and n < 0",
        "for i = 1, 10 do n = i if i == 3 then stop() end end",
        "for _, v in ipairs({1, 2, 3, 4, 5}) do n = v if v == 3 then stop() end end",
        "for k in next, {1, 2, 3, 4, 5} do n = k if k == 3 then stop() end end",
        "local function it(_, k) if k < 9 then return k + 1 end end
         for k in it, nil, 0 do n = k if k == 3 then stop() end end",
    ] {
        let mut state = interrupted(source);
        assert_eq!(support::global_number(&mut state, "n"), 3.0, "{source}");
    }
}

#[test]
fn recursion_without_loops_stops_at_the_next_call() {
    let source = "local function f(k) n = k if k == 3 then stop() end return f(k + 1) + 1 end
                  f(1)";
    let mut state = interrupted(source);
    assert_eq!(support::global_number(&mut state, "n"), 3.0);
}

#[test]
fn a_lua_call_is_refused_before_its_body_runs() {
    let source = "local function f() ran = true end
                  stop()
                  f()";
    let mut state = interrupted(source);
    assert!(global_is_nil(&mut state, "ran"));
}

#[test]
fn a_call_from_rust_into_lua_is_refused_too() {
    // An `__index` function runs through a call made from Rust.
    let source = "local t = setmetatable({}, {__index = function() ran = true end})
                  stop()
                  local _ = t.k";
    let mut state = interrupted(source);
    assert!(global_is_nil(&mut state, "ran"));
}

#[test]
fn the_request_is_sticky_until_reset() {
    let mut state = interrupted("stop() while true do end");
    // A later call is refused at its first safepoint, before any of it runs.
    let source = "ran = true";
    assert_interrupted(support::try_run(&mut state, source), source);
    assert!(global_is_nil(&mut state, "ran"));
    state.interrupt().reset();
    support::run(&mut state, source);
    assert!(!global_is_nil(&mut state, "ran"));
}

#[test]
fn a_swallowed_interrupt_stops_the_caller_at_its_next_safepoint() {
    let source = "swallow(function() stop() while true do end end)
                  after = 1
                  while true do end";
    let mut state = interrupted(source);
    assert_eq!(support::global_number(&mut state, "after"), 1.0);
}

#[test]
fn an_interrupted_state_runs_again_after_reset() {
    let mut state = interrupted("t = {} for i = 1, 100 do t[i] = i if i == 50 then stop() end end");
    state.interrupt().reset();
    support::eval(
        &mut state,
        "local s = 0 for i = 1, #t do s = s + t[i] end return s",
        1,
    );
    assert_eq!(state.to_number(-1).expect("a number"), 1275.0);
}

/// The poll comes before the cursor moves, so the error carries the jumping
/// instruction's line. A poll after the jump would name the instruction
/// before the jump target instead: line 3 for the `while` (its body's first
/// line), line 1 for the `for` (its `FOR_PREP`).
#[test]
fn the_error_names_the_jumping_instruction() {
    for (source, line) in [
        // The while loop's backward JMP carries the last body line.
        (
            "local n = 0\nwhile true do\n  n = n + 1\n  if n == 2 then stop() end\nend\n",
            4,
        ),
        // FOR_LOOP carries the `end` line.
        ("for i = 1, 3 do\n  if i == 2 then stop() end\nend\n", 3),
    ] {
        let mut state = state();
        let error = assert_interrupted(support::try_run(&mut state, source), source);
        assert_eq!(error.line_num(), line, "{source}");
    }
}

/// Admission refuses a callee reached through a `__call` chain: the error
/// sits on the caller's line, the handler never ran and is absent from the
/// trace, and after a reset the same State makes the call.
#[test]
fn a_call_chain_admission_is_refused_and_recovers() {
    let source =
        "callable = setmetatable({}, {__call = function() ran = true end})\nstop()\ncallable()\n";
    let mut state = state();
    let error = assert_interrupted(support::try_run(&mut state, source), source);
    assert_eq!(error.line_num(), 3);
    assert_eq!(error.stack_trace().len(), 1, "only the main chunk: {error}");
    assert!(global_is_nil(&mut state, "ran"));
    state.interrupt().reset();
    support::run(&mut state, "callable()");
    assert!(!global_is_nil(&mut state, "ran"));
}

/// Library work whose trip count the script chooses stops between
/// iterations. A function handler triggers on its third access and swaps
/// itself for a table handler, so no later access admits a Lua call: only
/// the library loop's own poll can stop it. Without that poll each call runs
/// to completion (or fails some other way) and is not interrupted.
#[test]
fn library_loops_stop_between_iterations() {
    let handler = "reads = 0
        local values = {}
        for i = 0, 11 do values[i] = 'x' end
        local mt = {__len = function() return 10 end}
        function mt.__index(_, k)
            reads = reads + 1
            if reads == 3 then stop() mt.__index = values end
            return 'x'
        end
        t = setmetatable({}, mt)";
    for call in [
        "table.concat(t, '', 1, 10)",
        "table.move(t, 1, 10, 1, {})",
        "table.insert(t, 1, 'y')",
        "table.remove(t, 1)",
        "table.sort(t)",
    ] {
        let source = format!("{handler}\n{call}");
        let mut state = interrupted(&source);
        assert_eq!(support::global_number(&mut state, "reads"), 3.0, "{call}");
    }
}

/// A comparator that triggers stops the sort at its next comparison. (The
/// refusal comes from that comparator's admission; the heap sort's own polls
/// are what stop a default-comparator sort, which
/// `native_work_polls_without_running_lua` covers.)
#[test]
fn a_comparator_stops_the_sort_at_the_next_comparison() {
    let source = "compares = 0
        local t = {}
        for i = 1, 64 do t[i] = (i * 37) % 64 end
        table.sort(t, function(a, b)
            compares = compares + 1
            if compares == 5 then stop() end
            return a < b
        end)";
    let mut state = interrupted(source);
    assert_eq!(support::global_number(&mut state, "compares"), 5.0);
}

/// Native work that no Lua runs inside polls too: with the request already
/// set when the library function starts, it fails without producing a
/// result.
#[test]
fn native_work_polls_without_running_lua() {
    for call in [
        "string.find(s, 'ab', 1, true)",
        "string.find(s, 'a+b')",
        "string.match(s, '(a*)b')",
        "string.gsub(s, '', 'x')",
        "string.gsub(s, 'a', 'b')",
        "string.gsub(s, 'a+', 'b')",
        "table.move({1, 2, 3}, 1, 3, 2)",
        "table.sort({3, 1, 2})",
        "table.concat({'a', 'b'})",
    ] {
        let source = format!("local s = 'aaaaab'\nstop()\nresult = {call}");
        let mut state = interrupted(&source);
        assert!(global_is_nil(&mut state, "result"), "{call}");
    }
}

#[test]
fn a_gmatch_iterator_stops() {
    let source = "for w in string.gmatch('one two three', '%a+') do n = (n or 0) + 1 stop() end";
    let mut state = interrupted(source);
    assert_eq!(support::global_number(&mut state, "n"), 1.0);
}

/// A pending interrupt stops `table.move` at callback entry, before it
/// prepays its range or copies anything: the destination stays empty and two
/// runs that differ only in the range length cost the same.
#[test]
fn an_interrupted_raw_move_keeps_its_destination_and_prepaid_cost() {
    let run = |last: u32| {
        let mut state = state();
        support::run(&mut state, "dest = {}");
        let source = format!("stop() table.move({{1, 2, 3, 4}}, 1, {last}, 1, dest)");
        let before = state.cost_used();
        assert_interrupted(support::try_run(&mut state, &source), &source);
        let charged = state.cost_used() - before;
        state.interrupt().reset();
        support::eval(&mut state, "return #dest", 1);
        assert_eq!(state.to_number(-1).expect("a number"), 0.0, "{source}");
        charged
    };
    assert_eq!(run(4), run(1));
}

#[test]
fn one_handle_stops_every_state_that_shares_it() {
    let shared = Interrupt::new();
    let mut states: Vec<State> = (0..2)
        .map(|_| State::builder().interrupt(shared.clone()).build())
        .collect();
    shared.trigger();
    for state in &mut states {
        assert!(state.interrupt().same_as(&shared));
        assert_interrupted(support::try_run(state, "x = 1"), "x = 1");
    }
    shared.reset();
    for state in &mut states {
        support::run(state, "x = 1");
    }
}

#[test]
fn a_replaced_handle_takes_over() {
    let mut state = state();
    let old = state.interrupt();
    old.trigger();
    let fresh = Interrupt::new();
    state.set_interrupt(fresh.clone());
    support::run(&mut state, "x = 1");
    fresh.trigger();
    assert_interrupted(support::try_run(&mut state, "x = 2"), "x = 2");
    assert!(
        old.is_triggered(),
        "the old handle is the host's, untouched"
    );
}

#[test]
fn a_fresh_state_has_its_own_untriggered_handle() {
    let a = State::new();
    let b = State::new();
    assert!(!a.interrupt().is_triggered());
    assert!(!a.interrupt().same_as(&b.interrupt()));
    assert!(a.interrupt().same_as(&a.interrupt()));
}

#[cfg(feature = "snapshot")]
#[test]
fn a_restored_state_joins_a_host_handle_through_setup() {
    let mut original = State::new();
    support::run(&mut original, "function spin() while true do end end");
    let save = original.save_state().expect("the State saves");
    let shared = Interrupt::new();
    let handle = shared.clone();
    let mut restored = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        move |state: &mut State| -> Result<(), std::convert::Infallible> {
            state.set_interrupt(handle);
            Ok(())
        },
    )
    .expect("the save loads");
    shared.trigger();
    restored.get_global("spin").expect("spin is readable");
    let result = restored.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    assert_interrupted(result, "spin()");
}
