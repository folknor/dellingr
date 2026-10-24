//! Fail-fast error-boundary and caught-recovery tests.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support::Recorder;
use crate::support::error_boundary_cases::{CASES, boundary_transcript, fail, natural, run};

#[test]
fn error_boundary_budget_sweep() {
    for (name, source) in CASES {
        let baseline = run(name, source, None, None);
        assert!(baseline.error.is_none(), "{name}: {:?}", baseline.error);
        // Outcomes are monotone in the budget: every budget below one threshold
        // fails with BudgetExceeded (asserted inside `run`), every budget at or
        // above it succeeds. Budgets are swept consecutively, so the last
        // failure sits directly below the threshold.
        let mut first_success = None;
        let mut last_failure = None;
        for budget in 0..=baseline.cost {
            let outcome = run(name, source, Some(i64::try_from(budget).unwrap()), None);
            if outcome.error.is_none() {
                if first_success.is_none() {
                    first_success = Some(budget);
                }
            } else {
                if let Some(succeeded_at) = first_success {
                    panic!("{name}: budget {budget} failed after budget {succeeded_at} succeeded");
                }
                last_failure = Some((budget, outcome.cost));
            }
        }

        let threshold = first_success.expect("the baseline cost must allow execution");
        assert!(threshold > 0, "{name}: zero budget must fail");
        let (last_failed_budget, cost_used) = last_failure.expect("zero budget must fail");
        // The signed remaining lets a charge that starts positive cross zero, so
        // the run one unit short spends its whole budget and stops at the next
        // costed operation.
        assert_eq!(
            cost_used, last_failed_budget,
            "{name}: the last failed run must spend exactly its budget"
        );
    }
}

#[test]
fn error_boundary_transcript() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/error_boundary_transcript.txt");
    let expected = std::fs::read_to_string(fixture).expect(
        "generate the boundary fixture with brokkr run --debug regenerate_error_boundary -- tests/fixtures/error_boundary_transcript.txt",
    );
    assert_eq!(
        boundary_transcript(),
        expected,
        "error-boundary transcript changed"
    );
}

#[cfg(feature = "fault-injection")]
#[test]
fn error_boundary_fault_sweep() {
    for (name, source) in CASES {
        let baseline = run(name, source, None, Some(None));
        assert!(baseline.error.is_none(), "{name}: {:?}", baseline.error);
        let baseline = baseline.events;
        let count = baseline.iter().filter(|event| event.eligible).count();
        for target in 1..=count {
            eprintln!("{name}: refusal {target}/{count}");
            let events = run(
                name,
                source,
                None,
                Some(Some(u32::try_from(target).unwrap())),
            )
            .events;
            let fired = events.iter().position(|event| event.fired).unwrap();
            assert_eq!(
                events[..=fired]
                    .iter()
                    .filter(|event| event.eligible)
                    .count(),
                target,
                "{name}: wrong eligible event fired"
            );
            for index in 0..=fired {
                assert_eq!(
                    (
                        events[index].site,
                        events[index].argument,
                        events[index].eligible,
                    ),
                    (
                        baseline[index].site,
                        baseline[index].argument,
                        baseline[index].eligible,
                    ),
                    "{name}: target identity changed"
                );
            }
        }
    }
}

#[cfg(test)]
fn empty(_: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[cfg(test)]
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

#[cfg(test)]
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
    for (name, source) in natural() {
        let outcome = run(name, &source, None, None);
        let error = outcome
            .error
            .unwrap_or_else(|| panic!("{name}: natural case succeeded: {}", outcome.transcript));
        let correct = match name {
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
        if name == "gsub_literal_size" {
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
