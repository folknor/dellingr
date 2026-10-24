//! Paired execution with the same build, varying only inline-cache hits.

use std::sync::{Arc, Mutex};

use crate::examples_corpus;
use crate::{ArgCount, HostCallbacks, Program, RetCount, State};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<Vec<u8>>>>);

impl HostCallbacks for Output {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        self.0
            .lock()
            .expect("output lock is not poisoned")
            .push(message.to_vec());
    }

    fn on_error(&mut self, _error: &crate::error::Error) {}
}

struct HitMode(bool);

impl HitMode {
    fn new(enabled: bool) -> Self {
        Self(super::caches::ENABLE_HITS.replace(enabled))
    }
}

impl Drop for HitMode {
    fn drop(&mut self) {
        super::caches::ENABLE_HITS.set(self.0);
    }
}

// A larger per-example ceiling gives ordinary programs room to reach repeated
// calls and cache hits, while keeping stress workloads and long debug kernels
// bounded under the differential check's time cap.
const COST_BUDGET: i64 = 100_000;
// A floor on completed examples, so a budget or corpus change cannot silently
// shrink the comparison back to prefixes. It was chosen as roughly half the
// corpus; revisit it with the budget when either moves.
const MIN_COMPLETED_EXAMPLES: usize = 40;

fn execute(program: &Program, enabled: bool) -> (Vec<Vec<u8>>, u64, Option<String>) {
    let _mode = HitMode::new(enabled);
    let output = Output::default();
    let mut state = State::with_callbacks(Box::new(output.clone()));
    state.set_cost_budget(COST_BUDGET);
    let result = state
        .load(program)
        .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)));
    let error = result.err().map(|error| error.to_string());
    let prints = output
        .0
        .lock()
        .expect("output lock is not poisoned")
        .clone();
    (prints, state.cost_used(), error)
}

#[test]
fn corpus_inline_cache_differential() {
    let paths = examples_corpus::example_corpus();
    let mut completed = 0;
    for path in &paths {
        let source = std::fs::read(path).expect("corpus source is readable");
        let name = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .expect("corpus source is in the project")
            .to_string_lossy();
        let program =
            Program::compile(&source, Some(name.as_bytes())).expect("corpus source compiles");
        let with_hits = execute(&program, true);
        let without_hits = execute(&program, false);
        assert_eq!(with_hits, without_hits, "{}", path.display());
        if with_hits.2.is_none() {
            completed += 1;
        }
    }
    assert!(
        completed >= MIN_COMPLETED_EXAMPLES,
        "only {completed} of {} examples completed with cost budget {COST_BUDGET}; \
         the floor is {MIN_COMPLETED_EXAMPLES}",
        paths.len()
    );
}
