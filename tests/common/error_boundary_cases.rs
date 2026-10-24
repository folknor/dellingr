//! Fail-fast error-boundary corpus, shared by budget and refusal sweeps.

use dellingr::error::Error;
use dellingr::{ArgCount, LuaType, RetCount, State};

#[path = "recorder.rs"]
mod recorder;
pub(crate) use recorder::Recorder;

pub(crate) const CASES: &[(&str, &str)] = &[
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

/// Cases that fail on their own, with no budget or injected refusal. The
/// count-limit cases reach exactly one past their limit: the called object
/// that `__call` prepends, or the leading fixed value, is the extra one.
pub(crate) fn natural() -> Vec<(&'static str, String)> {
    let max_arguments = dellingr::limits::MAX_ARGUMENTS;
    let max_results = dellingr::limits::MAX_RESULTS;
    let mut cases = vec![
        (
            "chain_arguments",
            format!(
                "local t = setmetatable({{}}, {{__call = function() end}}) return t(unpack({{}}, 1, {max_arguments}))"
            ),
        ),
        (
            "dynamic_arguments",
            format!("local function f() end f(1, unpack({{}}, 1, {max_arguments}))"),
        ),
        (
            "results",
            format!(
                "local function f(...) return 0, ... end return f(unpack({{}}, 1, {max_results}))"
            ),
        ),
    ];
    cases.extend(
        NATURAL_FIXED
            .iter()
            .map(|&(name, source)| (name, source.to_owned())),
    );
    cases
}

const NATURAL_FIXED: &[(&str, &str)] = &[
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

pub(crate) fn overreport(_: &mut State) -> dellingr::Result<u8> {
    Ok(1)
}
pub(crate) fn oversized(state: &mut State) -> dellingr::Result<u8> {
    state.push_bytes(vec![b'x'; dellingr::limits::MAX_STRING_BYTES + 1])?;
    Ok(1)
}

pub(crate) struct Outcome {
    pub(crate) cost: u64,
    pub(crate) transcript: String,
    #[cfg(feature = "fault-injection")]
    pub(crate) events: Vec<dellingr::FaultEvent>,
    pub(crate) error: Option<Error>,
}

fn primitive(state: &State, index: isize) -> String {
    match state.typ(index) {
        LuaType::Nil => "nil".into(),
        LuaType::Number => format!("number={:?}", state.to_number(index).unwrap()),
        LuaType::String => format!("bytes={:?}", state.to_bytes(index).unwrap()),
        other => panic!("unexpected corpus value: {other:?}"),
    }
}

pub(crate) fn run(
    name: &str,
    source: &str,
    budget: Option<i64>,
    fault: Option<Option<u32>>,
) -> Outcome {
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
            events.iter().filter(|event| event.fired).count() == 1,
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

// Both the suite and the regeneration tool use this corpus and runner.
pub(crate) fn boundary_transcript() -> String {
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
    for (name, source) in natural() {
        transcript.push_str(&format!("{name} natural\n"));
        transcript.push_str(&run(name, &source, None, None).transcript);
    }
    transcript
}

pub(crate) fn fail(_: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(
        dellingr::error::ErrorKind::HostError("nested failure".into()),
    ))
}
