//! Call-site traceback and batched-cost behavior for callback-capable opcodes.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

const SETUP: &str = r#"left = 1
right = 2
local function fail(...)
  local first = left + right
  local second = left + right
  local third = left + right
  error("boom")
end

callable = setmetatable({}, {__call = fail})
iterator = setmetatable({}, {__call = fail})
indexed = setmetatable({}, {__index = fail})
lengthy = setmetatable({}, {__len = fail})
new_indexed = setmetatable({}, {__newindex = fail})
other = {}
"#;

struct ReentryCase {
    name: &'static str,
    source: &'static str,
    budget: i64,
    expected_used: u64,
}

const CASES: [ReentryCase; 8] = [
    ReentryCase {
        name: "CALL with __call",
        source: "local prior = left + right\ncallable()",
        budget: 3,
        expected_used: 3,
    },
    ReentryCase {
        name: "TFOR_CALL with __call",
        source: "local prior = left + right\nfor item in iterator do end",
        budget: 3,
        expected_used: 3,
    },
    ReentryCase {
        name: "LENGTH with __len",
        source: "local prior = left + right\nlocal result = #lengthy",
        budget: 3,
        expected_used: 3,
    },
    ReentryCase {
        name: "GET_FIELD with __index",
        source: "local prior = left + right\nlocal result = indexed.field",
        budget: 3,
        expected_used: 3,
    },
    ReentryCase {
        name: "GET_TABLE with __index",
        source: "local prior = left + right\nlocal result = indexed[left]",
        budget: 3,
        expected_used: 3,
    },
    ReentryCase {
        name: "SET_FIELD with __newindex",
        source: "local prior = left + right\nnew_indexed.field = right",
        budget: 4,
        expected_used: 4,
    },
    ReentryCase {
        name: "SET_FIELD_AT with __newindex",
        source: "local prior = left + right\nother.field, new_indexed.field = left, right",
        budget: 4,
        expected_used: 4,
    },
    ReentryCase {
        name: "SET_TABLE with __newindex",
        source: "local prior = left + right\nnew_indexed[left] = right",
        budget: 4,
        expected_used: 4,
    },
];

#[test]
fn reentering_opcodes_preserve_call_sites_and_cost_prefixes() {
    for case in CASES {
        let mut state = State::new();
        dellingr::Program::compile(SETUP, Some(b"setup".as_slice()))
            .and_then(|program| state.load(&program))
            .expect("metatable setup compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("metatable setup succeeds");

        // The first addition leaves one cost unit pending. Table writes add a
        // second unit before their callback; budgets leave two further units
        // for the callback so the flush at the re-entry boundary is exercised.
        state.set_cost_budget(case.budget);
        dellingr::Program::compile(case.source, Some(b"reentry".as_slice()))
            .and_then(|program| state.load(&program))
            .expect("reentry source compiles");
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("the callback's third addition must exceed the budget");

        assert!(
            matches!(error.kind(), ErrorKind::BudgetExceeded { .. }),
            "{} returned {error}",
            case.name
        );
        let trace = error.stack_trace();
        assert_eq!(trace.len(), 2, "{}: {trace:?}", case.name);
        assert_eq!(
            trace[0].source.as_deref(),
            Some(b"setup".as_slice()),
            "{}",
            case.name
        );
        assert_eq!(trace[0].line, 6, "{}: {trace:?}", case.name);
        assert_eq!(
            trace[1].source.as_deref(),
            Some(b"reentry".as_slice()),
            "{}",
            case.name
        );
        assert_eq!(trace[1].line, 2, "{}: {trace:?}", case.name);
        assert_eq!(
            state.cost_used(),
            case.expected_used,
            "{} must charge the prefix through the last admitted addition",
            case.name
        );
    }
}
