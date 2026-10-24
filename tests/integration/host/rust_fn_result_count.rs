//! A Rust callback's reported result count is validated against its frame.
//! Over-reporting is a host bug and fails the call instead of being padded
//! with nil; the callback entry paths that can reach RustFn are covered below.

use std::collections::BTreeSet;

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, RetCount, State};

use crate::support::PushNewRustFn;
use crate::support::{self, Recorder};

/// Pushes one value and claims three.
fn over_report(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(7.0)?;
    Ok(3)
}

/// Pushes one value and reports more results than any callback below has
/// arguments plus pushes.
fn over_report_max(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(7.0)?;
    Ok(u8::MAX)
}

/// Generic-for iterator: pushes one value onto its two arguments, claims four.
fn over_report_iterator(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(7.0)?;
    Ok(4)
}

/// Pushes nothing and returns its two arguments.
fn return_args(_state: &mut State) -> dellingr::Result<u8> {
    Ok(2)
}

/// Pushes three values and returns only the top two.
fn return_top_two(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(1.0)?;
    state.push_number(2.0)?;
    state.push_number(3.0)?;
    Ok(2)
}

/// Calls `over_report` and checks the failure left this callback's frame
/// intact before propagating the error.
fn nested_over_report(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(42.0)?;
    state.push_new_rust_fn(over_report)?;
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("inner callback over-reports");
    if state.get_top() != 1 || state.to_number(1)? != 42.0 {
        return Err(Error::without_location(ErrorKind::HostError(
            "outer frame disturbed".into(),
        )));
    }
    Err(error)
}

fn assert_over_report(kind: &ErrorKind) {
    assert!(
        matches!(
            kind,
            ErrorKind::ResultCountExceedsStack {
                reported: 3,
                available: 1
            }
        ),
        "unexpected error kind: {kind:?}"
    );
}

fn assert_max_over_report(kind: &ErrorKind, available: usize) {
    match kind {
        ErrorKind::ResultCountExceedsStack {
            reported,
            available: actual,
        } => {
            assert_eq!(*reported, u8::MAX);
            assert_eq!(*actual, available);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

struct LuaOverReportRoute {
    edge: &'static str,
    script: &'static str,
    available: usize,
}

const LUA_OVER_REPORT_ROUTES: &[LuaOverReportRoute] = &[
    LuaOverReportRoute {
        edge: "index_call->call_unguarded",
        script: "local t = setmetatable({}, {__index = bad}); return t.x",
        available: 3,
    },
    LuaOverReportRoute {
        edge: "newindex_call->call_unguarded",
        script: "local t = setmetatable({}, {__newindex = bad}); t.x = 1",
        available: 4,
    },
    LuaOverReportRoute {
        edge: "begin_call->begin_callable_call",
        script: "local f = setmetatable({}, {__call = bad}); return f()",
        available: 2,
    },
    LuaOverReportRoute {
        edge: "begin_callable_call->invoke_rust_fn",
        script: "local f = setmetatable({}, {__call = bad}); return f()",
        available: 2,
    },
    LuaOverReportRoute {
        edge: "len_call->call_unguarded",
        script: "return #setmetatable({}, {__len = bad})",
        available: 3,
    },
    LuaOverReportRoute {
        edge: "tostring_rendering->call_unguarded",
        script: "return tostring(setmetatable({}, {__tostring = bad}))",
        available: 2,
    },
    LuaOverReportRoute {
        edge: "table_sort_less->call_unguarded",
        script: "table.sort({2, 1}, bad)",
        available: 3,
    },
    LuaOverReportRoute {
        edge: "append_function_replacement->call_unguarded:whole",
        script: "return string.gsub('a', 'a', bad)",
        available: 2,
    },
    LuaOverReportRoute {
        edge: "append_function_replacement->call_unguarded:captures",
        script: "return string.gsub('a', '(a)', bad)",
        available: 2,
    },
    LuaOverReportRoute {
        edge: "get_table_unguarded->call_unguarded",
        script: "local t = setmetatable({}, {__len = function() return 1 end, __index = bad}); return table.concat(t)",
        available: 3,
    },
    LuaOverReportRoute {
        edge: "set_table->call_unguarded",
        script: "table.insert(setmetatable({}, {__len = function() return 0 end, __newindex = bad}), 'x')",
        available: 4,
    },
    LuaOverReportRoute {
        edge: "sequence_len->call_unguarded",
        script: "table.insert(setmetatable({}, {__len = bad}), 'x')",
        available: 3,
    },
];

const EXISTING_OVER_REPORT_EDGES: &[&str] = &[
    // Direct host call, also used by the nested host callback below.
    "call->call_unguarded",
    "call_unguarded->begin_call",
    "begin_call->invoke_rust_fn",
    // A Rust callback called by Lua and the generic-for Rust iterator path.
    "call_from_lua->begin_call",
    "instr_tfor_call->instr_tfor_call_rust_fn",
    "instr_tfor_call_rust_fn->invoke_rust_fn",
    // The anchored host entry added below.
    "call_anchor_unguarded->call_unguarded",
];

fn native_call_inventory() -> toml::Value {
    toml::from_str(include_str!("../../../reference/native-calls.toml"))
        .expect("native call inventory is valid TOML")
}

fn inventory_rust_fn_entry_edges() -> BTreeSet<String> {
    let inventory = native_call_inventory();
    let edges = inventory
        .get("edge")
        .and_then(toml::Value::as_array)
        .expect("native call inventory has edge entries");

    let mut entry_edges = BTreeSet::new();
    for edge in edges {
        let Some(id) = edge.get("id").and_then(toml::Value::as_str) else {
            continue;
        };
        if edge.get("excluded").is_some() {
            continue;
        }
        let Some((_, target)) = id.split_once("->") else {
            continue;
        };
        let target = target.split(':').next().unwrap_or(target);
        // The generic-for call_unguarded arm handles non-RustFn iterators;
        // gmatch calls its fixed Lua wrapper here, not a user callback.
        let can_enter_rust_fn = (target == "call_unguarded"
            && !matches!(
                id,
                "instr_tfor_call->call_unguarded" | "push_gmatch_wrapper->call_unguarded"
            ))
            || matches!(
                target,
                "invoke_rust_fn" | "begin_call" | "begin_callable_call" | "instr_tfor_call_rust_fn"
            );
        if can_enter_rust_fn {
            entry_edges.insert(id.to_owned());
        }
    }
    entry_edges
}

fn inventory_scenario_edges() -> BTreeSet<String> {
    let inventory = native_call_inventory();
    let scenarios = inventory
        .get("scenario")
        .and_then(toml::Value::as_array)
        .expect("native call inventory has scenario entries");
    let segments = inventory
        .get("segment")
        .and_then(toml::Value::as_table)
        .expect("native call inventory has path segments");

    let mut edges = BTreeSet::new();
    for scenario in scenarios {
        let path = scenario
            .get("path")
            .and_then(toml::Value::as_array)
            .expect("each native call scenario has a path");
        for item in path {
            let item = item
                .as_str()
                .expect("native call scenario path entries are strings");
            if let Some(segment_name) = item.strip_prefix('@') {
                let segment = segments
                    .get(segment_name)
                    .and_then(toml::Value::as_array)
                    .expect("native call scenario names an existing segment");
                edges.extend(
                    segment
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned),
                );
            } else {
                edges.insert(item.to_owned());
            }
        }
    }
    edges
}

fn tested_rust_fn_entry_edges() -> BTreeSet<String> {
    LUA_OVER_REPORT_ROUTES
        .iter()
        .map(|route| route.edge.to_owned())
        .chain(
            EXISTING_OVER_REPORT_EDGES
                .iter()
                .map(|edge| (*edge).to_owned()),
        )
        .collect()
}

fn run_lua_over_report(script: &str, available: usize) {
    let (mut state, recorder) = Recorder::state();
    state
        .push_new_rust_fn(over_report_max)
        .expect("the callback should fit");
    state
        .set_global("bad")
        .expect("the callback should install");
    support::load(&mut state, script);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the Rust callback over-reports");
    assert_max_over_report(error.kind(), available);
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn inventory_rust_fn_entry_edges_have_result_count_coverage() {
    assert_eq!(
        tested_rust_fn_entry_edges(),
        inventory_rust_fn_entry_edges(),
        "update the over-report routes when an inventoried RustFn entry is added"
    );
}

#[test]
fn inventoried_lua_routes_reject_rust_fn_over_report() {
    let inventory_edges = inventory_rust_fn_entry_edges();
    let scenario_edges = inventory_scenario_edges();
    for route in LUA_OVER_REPORT_ROUTES {
        assert!(
            inventory_edges.contains(route.edge),
            "route edge is absent from the native call inventory: {}",
            route.edge
        );
        assert!(
            scenario_edges.contains(route.edge),
            "route edge has no native call inventory scenario: {}",
            route.edge
        );
        run_lua_over_report(route.script, route.available);
    }
}

#[test]
fn call_anchor_rejects_over_report() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_new_rust_fn(over_report_max)
        .expect("the callback should fit");
    let anchor = state
        .anchor_function()
        .expect("the callback can be anchored");
    let error = state
        .call_anchor(&anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the anchored callback over-reports");
    assert_max_over_report(error.kind(), 1);
    assert_eq!(recorder.error_count(), 1);
    assert_eq!(state.get_top(), 0);
}

#[test]
fn direct_host_call_rejects_over_report() {
    let (mut state, recorder) = Recorder::state();
    state.push_string("below").unwrap();
    state.push_new_rust_fn(over_report).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::All)
        .expect_err("over-report fails");
    assert_over_report(error.kind());
    assert_eq!(recorder.error_count(), 1);
    assert_eq!(state.get_top(), 1, "the call frame is cleared");
}

#[test]
fn lua_call_rejects_over_report() {
    let (mut state, recorder) = Recorder::state();
    state.push_new_rust_fn(over_report).unwrap();
    state.set_global("bad").unwrap();
    support::load(&mut state, "local a, b, c = bad()");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("over-report fails");
    assert_over_report(error.kind());
    assert_eq!(recorder.error_count(), 1);
    assert!(!error.stack_trace().is_empty());
}

#[test]
fn generic_for_iterator_rejects_over_report() {
    let (mut state, recorder) = Recorder::state();
    state.push_new_rust_fn(over_report_iterator).unwrap();
    state.set_global("bad").unwrap();
    support::load(&mut state, "for k, v in bad do error('unreachable') end");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("over-report fails");
    // The iterator receives (state, control) = (nil, nil), so its frame holds
    // the two arguments plus its one push.
    // Reporting 3 is therefore valid; the test iterator below reports 4.
    assert!(
        matches!(
            error.kind(),
            ErrorKind::ResultCountExceedsStack {
                reported: 4,
                available: 3
            }
        ),
        "unexpected error kind: {:?}",
        error.kind()
    );
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn nested_over_report_restores_outer_frame_and_reports_once() {
    let (mut state, recorder) = Recorder::state();
    state.push_new_rust_fn(nested_over_report).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("inner failure propagates");
    assert_over_report(error.kind());
    assert_eq!(recorder.error_count(), 1);
    assert_eq!(state.get_top(), 0);
}

#[test]
fn arguments_count_as_results() {
    let (mut state, _recorder) = Recorder::state();
    state.push_new_rust_fn(return_args).unwrap();
    state.push_number(1.0).unwrap();
    state.push_number(2.0).unwrap();
    state.call(ArgCount::Fixed(2), RetCount::All).unwrap();
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 1.0);
    assert_eq!(state.to_number(2).unwrap(), 2.0);
}

#[test]
fn under_report_keeps_top_values_in_order() {
    let (mut state, _recorder) = Recorder::state();
    state.push_new_rust_fn(return_top_two).unwrap();
    state.push_number(9.0).unwrap();
    state.call(ArgCount::Fixed(1), RetCount::All).unwrap();
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 2.0);
    assert_eq!(state.to_number(2).unwrap(), 3.0);
}

#[test]
fn under_report_discards_many_arguments_without_disturbing_caller() {
    let (mut state, _recorder) = Recorder::state();
    state.push_string("caller").unwrap();
    state.push_new_rust_fn(return_top_two).unwrap();
    for n in 0..200 {
        state.push_number(f64::from(n)).unwrap();
    }
    state.call(ArgCount::Fixed(200), RetCount::All).unwrap();
    assert_eq!(state.get_top(), 3);
    assert_eq!(state.to_string(1).unwrap(), "caller");
    assert_eq!(state.to_number(2).unwrap(), 2.0);
    assert_eq!(state.to_number(3).unwrap(), 3.0);
}

#[test]
fn caller_side_padding_is_unchanged() {
    let (mut state, _recorder) = Recorder::state();
    state.push_new_rust_fn(return_top_two).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.get_top(), 4);
    assert_eq!(state.to_number(1).unwrap(), 2.0);
    assert_eq!(state.to_number(2).unwrap(), 3.0);
    assert_eq!(state.typ(3), dellingr::LuaType::Nil);
    assert_eq!(state.typ(4), dellingr::LuaType::Nil);
}
