//! Paired execution with the same build, varying only inline-cache hits.
//!
//! Each program runs twice on fresh States from one compiled [`Program`]: once
//! with hits on, once with every cache read forced to miss (fills still
//! happen, so the second arm is the lookup path, not an empty cache). A run is
//! two phases. The first calls the chunk; the second, when the chunk left a
//! global `_bench` function, calls it again on the same State, so it reaches
//! sites the first phase warmed. Each phase compares prints, cost, the
//! rendered error and the returned values, and the State is saved after each
//! phase and the bytes compared, so a cache that corrupts only heap state a
//! later call or a snapshot would see fails here too.
//!
//! Limits: returned tables and functions compare by type only (their identity
//! is a pointer, and rendering them would assign saved identities); the
//! continuation is a new invocation, not a resumption of one the budget
//! stopped; a warm State against one restored cold from a snapshot is
//! `tests/integration/snapshot/corpus_roundtrip.rs`'s comparison, not this
//! one. The save comparison needs the `snapshot` feature, which the
//! `inline-cache-differential` check shape enables; without it every save
//! reports an error and the save floors fail.

use std::sync::{Arc, Mutex};

use super::caches::HitMode;
use crate::example_list::EXAMPLES;
use crate::{ArgCount, HostCallbacks, LuaType, Program, RetCount, State};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<Vec<u8>>>>);

impl Output {
    fn take(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.0.lock().expect("output lock is not poisoned"))
    }
}

impl HostCallbacks for Output {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        self.0
            .lock()
            .expect("output lock is not poisoned")
            .push(message.to_vec());
    }

    fn on_error(&mut self, _error: &crate::error::Error) {}
}

// A larger per-example ceiling gives ordinary programs room to reach repeated
// calls and cache hits, while keeping stress workloads and long debug kernels
// bounded under the differential check's time cap. Each phase gets it afresh.
const COST_BUDGET: i64 = 100_000;
// Floors over the corpus alone (the inline fixtures do not count), so a
// budget or corpus change cannot silently shrink the comparison back to
// prefixes, refusals or skipped continuations. They were set below the counts
// measured when they were introduced; revisit them with the budget when either
// moves.
const MIN_COMPLETED_EXAMPLES: usize = 40;
const MIN_CONTINUATIONS: usize = 35;
const MIN_COMPLETED_CONTINUATIONS: usize = 35;
const MIN_SAVED_EXAMPLES: usize = 80;

/// A returned value, observed without pointer identities.
#[derive(Debug, PartialEq)]
enum Returned {
    Nil,
    Boolean(bool),
    /// Bits of the public number read, so NaN is canonical and the sign of
    /// zero stays visible.
    Number(u64),
    String(Vec<u8>),
    /// Tables and functions: type only.
    Object(LuaType),
}

#[derive(Debug, PartialEq)]
struct Phase {
    prints: Vec<Vec<u8>>,
    cost: u64,
    error: Option<String>,
    returns: Vec<Returned>,
}

#[derive(Debug, PartialEq)]
struct Run {
    first: Phase,
    first_save: Result<Vec<u8>, String>,
    continuation: Option<Phase>,
    final_save: Result<Vec<u8>, String>,
}

/// Calls the function on top of the stack (or reports `pending`'s error) and
/// captures the phase, leaving the stack empty.
fn phase(state: &mut State, output: &Output, pending: crate::Result<()>) -> Phase {
    let result = pending.and_then(|()| state.call(ArgCount::Fixed(0), RetCount::All));
    let error = result.err().map(|error| error.to_string());
    let top = state.get_top();
    if error.is_some() {
        // Cleanup would hide partial results a failed call leaked.
        assert_eq!(top, 0, "a failed call left values on the stack");
    }
    let returns = (1..=top)
        .map(|index| {
            let index = isize::try_from(index).expect("a stack index fits isize");
            match state.typ(index) {
                LuaType::Nil => Returned::Nil,
                LuaType::Boolean => Returned::Boolean(state.to_boolean(index)),
                LuaType::Number => Returned::Number(
                    state
                        .to_number(index)
                        .expect("a number reads as a number")
                        .to_bits(),
                ),
                LuaType::String => Returned::String(
                    state
                        .to_bytes(index)
                        .expect("a string reads as bytes")
                        .to_vec(),
                ),
                other => Returned::Object(other),
            }
        })
        .collect();
    state.set_top(0).expect("emptying the stack succeeds");
    Phase {
        prints: output.take(),
        cost: state.cost_used(),
        error,
        returns,
    }
}

#[cfg(feature = "snapshot")]
fn save(state: &State) -> Result<Vec<u8>, String> {
    state
        .save_state()
        .map(|save| save.bytes)
        .map_err(|error| error.to_string())
}

#[cfg(not(feature = "snapshot"))]
fn save(_state: &State) -> Result<Vec<u8>, String> {
    Err("built without the snapshot feature".to_owned())
}

fn execute(program: &Program, enabled: bool) -> Run {
    let _mode = HitMode::new(enabled);
    let output = Output::default();
    let mut state = State::builder().callbacks(Box::new(output.clone())).build();
    state
        .set_cost_budget(COST_BUDGET)
        .expect("the State is metered");
    let loaded = state.load(program);
    let first = phase(&mut state, &output, loaded);
    let first_save = save(&state);
    state
        .set_cost_budget(COST_BUDGET)
        .expect("the State is metered");
    state
        .get_global("_bench")
        .expect("global lookup fits the empty stack");
    let continuation = if state.typ(-1) == LuaType::Function {
        Some(phase(&mut state, &output, Ok(())))
    } else {
        state.pop(1).expect("global lookup pushed one value");
        None
    };
    let final_save = save(&state);
    Run {
        first,
        first_save,
        continuation,
        final_save,
    }
}

fn compare(name: &str, source: &[u8]) -> Run {
    let program = Program::compile(source, Some(name.as_bytes()))
        .unwrap_or_else(|error| panic!("{name}: compile: {error}"));
    let with_hits = execute(&program, true);
    let without_hits = execute(&program, false);
    assert_eq!(with_hits.first, without_hits.first, "{name}: first phase");
    assert_eq!(
        with_hits.first_save, without_hits.first_save,
        "{name}: save after the first phase"
    );
    assert_eq!(
        with_hits.continuation, without_hits.continuation,
        "{name}: continuation"
    );
    assert_eq!(
        with_hits.final_save, without_hits.final_save,
        "{name}: final save"
    );
    with_hits
}

#[test]
fn corpus_inline_cache_differential() {
    let mut completed = 0;
    let mut continuations = 0;
    let mut completed_continuations = 0;
    let mut saved = 0;
    for example in EXAMPLES {
        let source = std::fs::read(example.path).expect("corpus source is readable");
        let run = compare(example.relative, &source);
        if run.first.error.is_none() {
            completed += 1;
        }
        if let Some(continuation) = &run.continuation {
            continuations += 1;
            if continuation.error.is_none() {
                completed_continuations += 1;
            }
        }
        if run.first_save.is_ok() && run.final_save.is_ok() {
            saved += 1;
        }
    }
    let counts = format!(
        "of {} examples with cost budget {COST_BUDGET}: {completed} completed (floor \
         {MIN_COMPLETED_EXAMPLES}), {continuations} continued (floor {MIN_CONTINUATIONS}), \
         {completed_continuations} continuations completed (floor \
         {MIN_COMPLETED_CONTINUATIONS}), {saved} saved both times (floor {MIN_SAVED_EXAMPLES})",
        EXAMPLES.len()
    );
    assert!(completed >= MIN_COMPLETED_EXAMPLES, "{counts}");
    assert!(continuations >= MIN_CONTINUATIONS, "{counts}");
    assert!(
        completed_continuations >= MIN_COMPLETED_CONTINUATIONS,
        "{counts}"
    );
    assert!(saved >= MIN_SAVED_EXAMPLES, "{counts}");
}

/// The corpus has no top-level returns; this chunk returns one value of
/// every kind the comparison distinguishes.
#[test]
fn returned_values_match_with_and_without_hits() {
    let run = compare(
        "returns",
        br"
        local t = setmetatable({}, { __index = { k = 'via index' } })
        local function read() return t.k end
        read()
        return nil, true, false, 1.5, -0.0, 0/0, read(), t, read
        ",
    );
    let expected = vec![
        Returned::Nil,
        Returned::Boolean(true),
        Returned::Boolean(false),
        Returned::Number(1.5f64.to_bits()),
        Returned::Number((-0.0f64).to_bits()),
        Returned::Number(crate::numeral::CANONICAL_NAN_BITS),
        Returned::String(b"via index".to_vec()),
        Returned::Object(LuaType::Table),
        Returned::Object(LuaType::Function),
    ];
    assert_eq!(run.first.returns, expected);
    assert!(run.first_save.is_ok() && run.final_save.is_ok(), "{run:?}");
}

/// One method site warmed by the chunk, then revalidated in the continuation
/// after a method update and after an `__index` replacement, each result
/// recorded.
#[test]
fn a_continuation_revalidates_a_site_the_chunk_warmed() {
    let run = compare(
        "continuation",
        br"
        local methods_a = {}
        function methods_a:f() return 1 end
        local methods_b = {}
        function methods_b:f() return 3 end
        local mt = { __index = methods_a }
        local t = setmetatable({}, mt)
        local function call() return t:f() end
        warm = call()
        function _bench()
            function methods_a:f() return 2 end
            local updated = call()
            mt.__index = methods_b
            local replaced = call()
            return warm * 100 + updated * 10 + replaced
        end
        ",
    );
    let continuation = run.continuation.as_ref().expect("_bench is defined");
    assert_eq!(
        continuation.returns,
        vec![Returned::Number(123f64.to_bits())]
    );
    assert!(run.first_save.is_ok() && run.final_save.is_ok(), "{run:?}");
}
