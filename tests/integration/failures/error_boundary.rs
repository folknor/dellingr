//! Fail-fast error-boundary corpus, shared by budget and refusal sweeps.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support::Recorder;

const CASES: &[(&str, &str)] = &[
    ("lua", "local function f(x) return x + 1 end return f(7)"),
    (
        "varargs",
        "local function f(a, ...) local function g(x, ...) return x end return g(...) end return f(nil, 8, {})",
    ),
    (
        "chain",
        "local t = setmetatable({}, {__call = function() return 8 end}) return setmetatable({}, {__call = t})()",
    ),
    (
        "index",
        "local t = setmetatable({}, {__index = function() return 8 end}) return setmetatable({}, {__index = t}).x",
    ),
    (
        "newindex",
        "local t = setmetatable({}, {__newindex = function() observed = 8 end}) setmetatable({}, {__newindex = t}).x = 1 return observed",
    ),
    (
        "newindex_table",
        "local s = {} setmetatable({}, {__newindex = setmetatable({}, {__newindex = s})}).x = 8 return s.x",
    ),
    ("string", "return ('abc'):sub(1, 2)"),
    (
        "rust_iterator",
        "local n = 0 for _, v in ipairs({1, 2}) do n = n + v end return n",
    ),
    (
        "lua_iterator",
        "local n = 0 for v in function(_, x) if x < 2 then return x + 1 end end, nil, 0 do n = n + v end return n",
    ),
    (
        "sort",
        "local t = {3, 1, 2} table.sort(t, function(a, b) return a < b end) return t[1]",
    ),
    (
        "gsub",
        "return string.gsub('aaa', 'a', function() return 'b' end)",
    ),
    (
        "insert",
        "local t = setmetatable({}, {__newindex = function() observed = 8 end}) table.insert(t, 1) return 8",
    ),
    (
        "tostring",
        "return tostring(setmetatable({}, {__tostring = function() return 'ok' end}))",
    ),
    (
        "host_tostring",
        "return render(setmetatable({}, {__tostring = function() return 'ok' end}))",
    ),
];

fn render(state: &mut State) -> dellingr::Result<u8> {
    let rendered = state.tostring_bytes(1)?;
    state.push_bytes(rendered)?;
    Ok(1)
}

const NATURAL: &[(&str, &str)] = &[
    (
        "chain_arguments",
        "local t = setmetatable({}, {__call = function() end}) return t(unpack({}, 1, 255))",
    ),
    (
        "dynamic_arguments",
        "local function f() end f(1, unpack({}, 1, 255))",
    ),
    (
        "results",
        "local function f(...) return 0, ... end return f(unpack({}, 1, 255))",
    ),
    ("callback", "return fail()"),
    ("overreport", "return overreport()"),
    ("type", "return nil + 1"),
    ("argument", "return table.insert(false, 1)"),
    ("pattern", "return string.find('x', '%')"),
    ("library", "return next({}, 'missing')"),
    ("concat_size", "return large .. large"),
    ("format_size", "return string.format('%s%s', large, large)"),
    ("gsub_literal_size", "return string.gsub('aa', 'a', large)"),
    (
        "gsub_function_size",
        "return string.gsub('aa', 'a', function() return large end)",
    ),
    ("push_size", "return oversized()"),
];

fn overreport(_: &mut State) -> dellingr::Result<u8> {
    Ok(1)
}
fn oversized(state: &mut State) -> dellingr::Result<u8> {
    state.push_bytes(vec![b'x'; dellingr::limits::MAX_STRING_BYTES + 1])?;
    Ok(1)
}

struct Outcome {
    cost: u64,
    transcript: String,
    #[cfg(feature = "fault-injection")]
    events: Vec<(&'static str, usize, bool)>,
    error: Option<Error>,
}

fn primitive(state: &State, index: isize) -> String {
    match state.typ(index) {
        LuaType::Nil => "nil".into(),
        LuaType::Number => format!("number={:?}", state.to_number(index).unwrap()),
        LuaType::String => format!("bytes={:?}", state.to_bytes(index).unwrap()),
        other => panic!("unexpected corpus value: {other:?}"),
    }
}

fn run(name: &str, source: &str, budget: Option<i64>, fault: Option<Option<u32>>) -> Outcome {
    let (mut state, recorder) = Recorder::state();
    let callbacks: [(&str, dellingr::RustFunc); 4] = [
        ("render", render),
        ("fail", fail),
        ("overreport", overreport),
        ("oversized", oversized),
    ];
    for (name, callback) in callbacks {
        state.push_rust_fn(callback).unwrap();
        state.set_global(name).unwrap();
    }
    if source.contains("large") {
        state
            .push_bytes(vec![b'x'; dellingr::limits::MAX_STRING_BYTES / 2 + 1])
            .unwrap();
        state.set_global("large").unwrap();
    }
    state.push_number(1234.0).unwrap();
    state.push_string("survivor").unwrap();
    let program = dellingr::Program::compile(source, Some(b"boundary-corpus")).unwrap();
    state.load(&program).unwrap();
    if let Some(budget) = budget {
        state.set_cost_budget(budget);
    }
    #[cfg(feature = "fault-injection")]
    if let Some(target) = fault {
        state.arm_fault_injection(target);
    }
    #[cfg(not(feature = "fault-injection"))]
    let _ = fault;
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(1));
    #[cfg(feature = "fault-injection")]
    let events = state.finish_fault_injection();
    if budget.is_some()
        && let Err(error) = &result
    {
        assert!(
            matches!(
                error.kind(),
                dellingr::error::ErrorKind::BudgetExceeded { .. }
            ),
            "unexpected budget outcome: {error:?}"
        );
    }
    let seen = recorder.errors();
    let detail = match &result {
        Err(error) => format!(
            "display={error}\nkind={:?} source={:?} position={}:{} traceback={:?} vm_fault={:?}\n",
            error.kind(),
            error.source_name(),
            error.line_num(),
            error.column(),
            error.stack_trace(),
            error.vm_fault()
        ),
        Ok(()) => String::new(),
    };
    let mut transcript = format!("{result:?}\n{detail}notifications={seen:?}\n");
    let top = state.get_top();
    let values: Vec<_> = (1..=top).map(|i| primitive(&state, i as isize)).collect();
    state.get_global("observed").unwrap();
    let observed = primitive(&state, -1);
    state.pop(1).unwrap();
    let cost = state.cost_used();
    transcript.push_str(&format!(
        "top={top} values={values:?} observed={observed}\ncost={cost}\n"
    ));
    assert_eq!(state.to_number(1).unwrap(), 1234.0);
    assert_eq!(state.to_bytes(2).unwrap(), b"survivor");
    match &result {
        Ok(()) => {
            assert_eq!(state.get_top(), 3);
            assert!(seen.is_empty(), "{name}: {seen:?}");
            match name {
                "string" => assert_eq!(state.to_bytes(3).unwrap(), b"ab"),
                "gsub" => assert_eq!(state.to_bytes(3).unwrap(), b"bbb"),
                "tostring" | "host_tostring" => assert_eq!(state.to_bytes(3).unwrap(), b"ok"),
                _ => {
                    let expected = match name {
                        "rust_iterator" | "lua_iterator" => 3.0,
                        "sort" => 1.0,
                        _ => 8.0,
                    };
                    assert_eq!(state.to_number(3).unwrap(), expected, "{name}");
                }
            }
            if matches!(name, "newindex" | "insert") {
                assert_eq!(observed, "number=8.0", "{name}");
            }
        }
        Err(error) => {
            assert_eq!(state.get_top(), 2, "{source}: {error}");
            assert_eq!(seen, vec![error.to_string()], "{source}");
        }
    }
    #[cfg(feature = "fault-injection")]
    if let Some(Some(target)) = fault {
        assert!(
            events.iter().filter(|event| event.2).count() == 1,
            "unreached target {target}: {events:?}"
        );
        assert!(
            result.is_err(),
            "injected refusal must propagate: {events:?}"
        );
    }
    state.set_top(0).unwrap();
    state.set_cost_budget(i64::MAX);
    #[cfg(feature = "snapshot")]
    {
        state.push_nil().unwrap();
        state.set_global("render").unwrap();
        for name in ["fail", "overreport", "oversized"] {
            state.push_nil().unwrap();
            state.set_global(name).unwrap();
        }
        state.save_state().unwrap();
    }
    recorder.clear();
    let reuse = dellingr::Program::compile("return 7", None).unwrap();
    state.load(&reuse).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
    let reuse_notifications = recorder.errors();
    assert!(reuse_notifications.is_empty(), "{name}: reuse notified");
    transcript.push_str(&format!("reuse=7 notifications={reuse_notifications:?}\n"));
    Outcome {
        cost,
        transcript,
        #[cfg(feature = "fault-injection")]
        events,
        error: result.err(),
    }
}

#[test]
fn error_boundary_budget_sweep() {
    for (name, source) in CASES {
        let baseline = run(name, source, None, None);
        assert!(baseline.error.is_none(), "{name}: {:?}", baseline.error);
        for budget in 0..=baseline.cost {
            run(name, source, Some(i64::try_from(budget).unwrap()), None);
        }
    }
}

#[test]
#[ignore = "on-demand transcript for before/after comparison"]
#[cfg(all(
    debug_assertions,
    not(any(
        feature = "snapshot",
        feature = "fault-injection",
        feature = "hotpath",
        feature = "hotpath-alloc"
    ))
))]
fn error_boundary_transcript() {
    let mut transcript = String::new();
    for (name, source) in CASES {
        let baseline = run(name, source, None, None);
        assert!(baseline.error.is_none(), "{name}: {:?}", baseline.error);
        for budget in 0..=baseline.cost {
            transcript.push_str(&format!("{name} budget={budget}\n"));
            transcript.push_str(
                &run(name, source, Some(i64::try_from(budget).unwrap()), None).transcript,
            );
        }
    }
    for (name, source) in NATURAL {
        transcript.push_str(&format!("{name} natural\n"));
        transcript.push_str(&run(name, source, None, None).transcript);
    }
    crate::support::scratch::write_boundary_transcript(transcript.as_bytes()).unwrap();
}

#[cfg(feature = "fault-injection")]
#[test]
#[ignore = "on-demand single-refusal sweep"]
fn error_boundary_fault_sweep() {
    for (name, source) in CASES {
        let baseline = run(name, source, None, Some(None));
        assert!(baseline.error.is_none(), "{name}: {:?}", baseline.error);
        let baseline = baseline.events;
        let count = baseline
            .iter()
            .filter(|(site, argument, _)| *site != "native_depth" || *argument > 0)
            .count();
        for target in 1..=count {
            eprintln!("{name}: refusal {target}/{count}");
            let events = run(
                name,
                source,
                None,
                Some(Some(u32::try_from(target).unwrap())),
            )
            .events;
            let fired = events.iter().position(|event| event.2).unwrap();
            assert_eq!(
                events[..=fired]
                    .iter()
                    .filter(|(site, argument, _)| *site != "native_depth" || *argument > 0)
                    .count(),
                target,
                "{name}: wrong eligible event fired"
            );
            for index in 0..=fired {
                assert_eq!(
                    (events[index].0, events[index].1),
                    (baseline[index].0, baseline[index].1),
                    "{name}: target identity changed"
                );
            }
        }
    }
}

fn fail(_: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(
        dellingr::error::ErrorKind::HostError("nested failure".into()),
    ))
}

fn empty(_: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

fn recover(state: &mut State) -> dellingr::Result<u8> {
    let original = state.get_top();
    let arguments = (1..=original)
        .map(|i| state.anchor_at(i as isize))
        .collect::<dellingr::Result<Vec<_>>>()?;
    let sentinel = state.to_number(-3)?;
    let mode = *state.user_data::<u8>().unwrap();
    match mode {
        0 => {
            state.push_rust_fn(fail)?;
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("nested callback fails");
        }
        1 => {
            state.get_global("bad_index")?;
            state.push_string("missing")?;
            state.get_table(-2).expect_err("chained handler fails");
            state.pop(1)?;
        }
        2 => {
            state.get_global("bad_chain")?;
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("terminal callback fails");
        }
        3 => {
            // Leave room for resolution and callback entry, but not 254
            // adjusted results. Filler belongs to this caller throughout.
            state.set_top((dellingr::limits::MAX_STACK_SIZE - 16) as isize)?;
            state.push_string("padding survivor")?;
            let top = state.get_top();
            state.get_global("empty_chain")?;
            let error = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(254))
                .expect_err("padding exceeds cap");
            assert!(matches!(
                error.kind(),
                dellingr::error::ErrorKind::StackOverflow { .. }
            ));
            assert_eq!(state.get_top(), top);
            assert_eq!(state.to_bytes(-1)?, b"padding survivor");
            for i in original + 1..top {
                assert_eq!(state.typ(i as isize), LuaType::Nil, "filler slot {i}");
            }
            state.set_top(original as isize)?;
        }
        _ => unreachable!(),
    }
    assert_eq!(state.get_top(), original);
    for (i, value) in arguments.iter().enumerate() {
        state.push_anchor(value)?;
        assert!(state.raw_equal(i as isize + 1, -1), "argument {}", i + 1);
        state.pop(1)?;
    }
    state.get_global("at_limit")?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
    assert_eq!(state.to_number(-1)?, 9.0);
    state.pop(1)?;
    state.get_global("past_limit")?;
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("one extra chain unit fails");
    assert!(matches!(
        error.kind(),
        dellingr::error::ErrorKind::MetamethodDepthExceeded { .. }
    ));
    assert_eq!(state.get_top(), original);
    state.push_number(sentinel)?;
    Ok(1)
}

#[test]
fn error_boundary_caught_recovery_preserves_outer_owners() {
    for chained in [false, true] {
        for mode in 0..4_u8 {
            let mut state = State::new();
            state.set_user_data(mode);
            let callbacks: [(&str, dellingr::RustFunc); 3] =
                [("recover", recover), ("fail", fail), ("empty", empty)];
            for (name, function) in callbacks {
                state.push_rust_fn(function).unwrap();
                state.set_global(name).unwrap();
            }
            let depth = dellingr::limits::MAX_METAMETHOD_DEPTH + if chained { 0 } else { 1 };
            let setup = format!("function wrap(f, n) for i = 1, n do f = setmetatable({{}}, {{__call = f}}) end return f end
                bad_index = setmetatable({{}}, {{__index = setmetatable({{}}, {{__index = fail}})}})
                bad_chain = wrap(fail, 3) empty_chain = wrap(empty, 3)
                at_limit = wrap(function() return 9 end, {depth})
                past_limit = wrap(function() return 9 end, {depth} + 1)
                outer = {}", if chained { "wrap(recover, 2)" } else { "recover" });
            let program = dellingr::Program::compile(setup, None).unwrap();
            state.load(&program).unwrap();
            state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
            state.get_global("outer").unwrap();
            state.push_number(73.0).unwrap();
            state.push_string("caller argument").unwrap();
            state.new_table().unwrap();
            state.call(ArgCount::Fixed(3), RetCount::Fixed(1)).unwrap();
            assert_eq!(state.to_number(-1).unwrap(), 73.0);
            state.set_top(0).unwrap();
            assert_full_chain_reuse(&mut state);
            #[cfg(feature = "snapshot")]
            {
                for name in [
                    "recover",
                    "fail",
                    "empty",
                    "outer",
                    "bad_index",
                    "bad_chain",
                    "empty_chain",
                ] {
                    state.push_nil().unwrap();
                    state.set_global(name).unwrap();
                }
                state.save_state().unwrap();
            }
        }
    }
}

fn assert_full_chain_reuse(state: &mut State) {
    let source = format!(
        "local t = setmetatable({{}}, {{__call = function() return 9 end}})
         for i = 1, {} do t = setmetatable({{}}, {{__call = t}}) end
         return t()",
        dellingr::limits::MAX_METAMETHOD_DEPTH
    );
    let program = dellingr::Program::compile(source, None).unwrap();
    state.load(&program).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 9.0);
    state.pop(1).unwrap();
}

#[test]
fn error_boundary_natural_failures() {
    for (name, source) in NATURAL {
        let outcome = run(name, source, None, None);
        let error = outcome
            .error
            .unwrap_or_else(|| panic!("{name}: natural case succeeded: {}", outcome.transcript));
        let correct = match *name {
            "chain_arguments" | "dynamic_arguments" => {
                matches!(error.kind(), ErrorKind::TooManyArguments { .. })
            }
            "results" => matches!(error.kind(), ErrorKind::TooManyResults { .. }),
            "callback" => matches!(error.kind(), ErrorKind::HostError(_)),
            "overreport" => matches!(
                error.kind(),
                ErrorKind::ResultCountExceedsStack {
                    reported: 1,
                    available: 0
                }
            ),
            "type" => matches!(error.kind(), ErrorKind::TypeError(_)),
            "argument" => matches!(error.kind(), ErrorKind::ArgError(_)),
            "pattern" => matches!(error.kind(), ErrorKind::PatternError(_)),
            "library" => matches!(error.kind(), ErrorKind::LibraryError(_)),
            "concat_size" | "format_size" | "gsub_literal_size" | "gsub_function_size"
            | "push_size" => matches!(error.kind(), ErrorKind::StringSizeExceeded { .. }),
            _ => panic!("no expected error kind for {name}"),
        };
        assert!(correct, "{name}: unexpected error {error:?}");
        if *name == "gsub_literal_size" {
            let cap = dellingr::limits::MAX_STRING_BYTES;
            assert!(
                matches!(error.kind(), ErrorKind::StringSizeExceeded { size, limit }
                if *size == cap + 1 && *limit == cap)
            );
            // Two replacement scans, two search comparisons, the four-unit
            // gsub entry charge, and exactly the accepted byte prefix.
            assert_eq!(outcome.cost, (cap + 2 * (cap / 2 + 1) + 6) as u64);
        }
    }
}

#[test]
fn error_boundary_pending_base_and_captured_local_survive_unwind() {
    for pending in ["sink(1, bad())", "local t = {1, bad()}"] {
        let mut state = State::new();
        let source = format!(
            "function exercise()
            local captured = 73
            saved = function() return captured end
            local function sink(...) end
            local inner = setmetatable({{}}, {{__call = function() error('stop') end}})
            local bad = setmetatable({{}}, {{__call = inner}})
            {pending}
            end"
        );
        let program = dellingr::Program::compile(source, Some(b"pending-capture")).unwrap();
        state.load(&program).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
        for _ in 0..2 {
            state.get_global("exercise").unwrap();
            let error = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("nested call fails");
            assert!(matches!(
                error.kind(),
                dellingr::error::ErrorKind::ScriptError { .. }
            ));
            assert_eq!(state.get_top(), 0);
            state.get_global("saved").unwrap();
            state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
            assert_eq!(state.to_number(-1).unwrap(), 73.0);
            state.set_top(0).unwrap();
            assert_full_chain_reuse(&mut state);
            #[cfg(feature = "snapshot")]
            state.save_state().unwrap();
        }
    }
}

#[test]
fn error_boundary_chained_result_padding_fits_a_lua_caller_and_reports_to_a_host() {
    let names = (0..64)
        .map(|i| format!("v{i}"))
        .collect::<Vec<_>>()
        .join(",");
    for kind in ["lua", "rust", "host"] {
        let (mut state, recorder) = Recorder::state();
        state.push_rust_fn(empty).unwrap();
        state.set_global("empty").unwrap();
        let terminal = if kind == "rust" {
            "empty"
        } else {
            "function() end"
        };
        let source = format!(
            "local inner = setmetatable({{}}, {{__call = {terminal}}})
            chain = setmetatable({{}}, {{__call = inner}})
            function caller()
                local captured = 73
                saved = function() return captured end
                local {names} = chain()
            end"
        );
        let program = dellingr::Program::compile(source, Some(b"padding")).unwrap();
        state.load(&program).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
        let prefix = dellingr::limits::MAX_STACK_SIZE - if kind == "host" { 4 } else { 100 };
        state.set_top(prefix as isize).unwrap();
        state.push_number(73.0).unwrap();
        state
            .get_global(if kind == "host" { "chain" } else { "caller" })
            .unwrap();
        let result = state.call(
            ArgCount::Fixed(0),
            RetCount::Fixed(if kind == "host" { 64 } else { 0 }),
        );
        if kind != "host" {
            // A Lua caller reserved its whole frame on entry, so its 64
            // result registers exist before the chain runs: padding them
            // needs no stack growth and cannot overflow.
            result.unwrap_or_else(|error| panic!("{kind}: {error:?}"));
            assert!(recorder.errors().is_empty());
            assert_eq!(state.get_top(), prefix + 1);
            state.set_top(0).unwrap();
            assert_full_chain_reuse(&mut state);
            state.get_global("saved").unwrap();
            state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
            assert_eq!(state.to_number(-1).unwrap(), 73.0);
            state.set_top(0).unwrap();
        } else {
            // A host asking for 64 results pads on the shared stack.
            let error = result.expect_err("padding fails after callee returned");
            assert!(
                matches!(
                    error.kind(),
                    dellingr::error::ErrorKind::StackOverflow { .. }
                ),
                "{kind}: {error:?}"
            );
            assert!(error.stack_trace().is_empty());
            assert!(error.source_name().is_none());
            assert_eq!(recorder.errors(), vec![error.to_string()]);
            assert_eq!(state.get_top(), prefix + 1);
            assert_eq!(state.to_number(-1).unwrap(), 73.0);
            state.set_top(0).unwrap();
            assert_full_chain_reuse(&mut state);
        }
        #[cfg(feature = "snapshot")]
        {
            // Remove the test callback and every closure/table retaining it.
            for name in ["empty", "chain", "caller"] {
                state.push_nil().unwrap();
                state.set_global(name).unwrap();
            }
            state.save_state().unwrap();
        }
    }
}
