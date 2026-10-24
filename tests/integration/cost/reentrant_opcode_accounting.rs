//! Call-site traceback and batched-cost behavior for callback-capable opcodes.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use super::cost_golden::pinned_cost;

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
    /// The dispatch opcode whose re-entry this case exercises.
    opcode: &'static str,
    source: &'static str,
    budget: i64,
}

const CASES: [ReentryCase; 9] = [
    ReentryCase {
        name: "CALL with __call",
        opcode: "CALL",
        source: "local prior = left + right\ncallable()",
        budget: 3,
    },
    ReentryCase {
        name: "TFOR_CALL with __call",
        opcode: "TFOR_CALL",
        source: "local prior = left + right\nfor item in iterator do end",
        budget: 3,
    },
    ReentryCase {
        name: "LENGTH with __len",
        opcode: "LEN",
        source: "local prior = left + right\nlocal result = #lengthy",
        budget: 3,
    },
    ReentryCase {
        name: "GET_FIELD with __index",
        opcode: "GET_FIELD",
        source: "local prior = left + right\nlocal result = indexed.field",
        budget: 3,
    },
    ReentryCase {
        name: "SELF with __index",
        opcode: "SELF",
        source: "local prior = left + right\nlocal result = indexed:method()",
        budget: 3,
    },
    ReentryCase {
        name: "GET_TABLE with __index",
        opcode: "GET_TABLE",
        source: "local prior = left + right\nlocal result = indexed[left]",
        budget: 3,
    },
    ReentryCase {
        name: "SET_FIELD with __newindex",
        opcode: "SET_FIELD",
        source: "local prior = left + right\nnew_indexed.field = right",
        budget: 4,
    },
    ReentryCase {
        name: "multiple-assignment field store with __newindex",
        opcode: "SET_FIELD",
        source: "local prior = left + right\nother.field, new_indexed.field = left, right",
        budget: 4,
    },
    ReentryCase {
        name: "SET_TABLE with __newindex",
        opcode: "SET_TABLE",
        source: "local prior = left + right\nnew_indexed[left] = right",
        budget: 4,
    },
];

#[test]
fn reentrant_case_sources_cover_reentry_opcodes() {
    const REQUIRED_OPCODES: &[&str] = &[
        "CALL",
        "TFOR_CALL",
        "LEN",
        "GET_FIELD",
        "GET_TABLE",
        "SET_FIELD",
        "SET_TABLE",
        "SELF",
    ];

    // Each case must emit the opcode it is named for, so editing one case
    // cannot hide behind another case that happens to emit the same opcode.
    for case in CASES {
        let program = dellingr::Program::compile(case.source, Some(case.name.as_bytes()))
            .unwrap_or_else(|error| panic!("{} source compiles: {error}", case.name));
        let listing = program.disassemble().to_string();
        let emitted: Vec<&str> = listing.lines().filter_map(listed_opcode).collect();
        assert!(
            emitted.contains(&case.opcode),
            "{} must emit {}; found {emitted:?}",
            case.name,
            case.opcode
        );
    }

    for &opcode in REQUIRED_OPCODES {
        assert!(
            CASES.iter().any(|case| case.opcode == opcode),
            "reentrant cases must include {opcode}"
        );
    }
}

fn listed_opcode(line: &str) -> Option<&str> {
    let instruction = line.split_once("] line ")?.1;
    let (_, instruction) = instruction.split_once("  ")?;
    instruction.split_ascii_whitespace().next()
}

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
            pinned_cost(&format!("reentry:{}", case.name)),
            "{} must charge the prefix through the last admitted addition",
            case.name
        );
    }
}
