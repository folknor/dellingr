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

// The corpus includes stress workloads. Compare bounded prefixes as well as
// completed programs so those workloads do not dominate a debug check.
const COST_BUDGET: i64 = 10_000;

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
    for path in examples_corpus::example_corpus() {
        let source = std::fs::read(&path).expect("corpus source is readable");
        let name = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .expect("corpus source is in the project")
            .to_string_lossy();
        let program =
            Program::compile(&source, Some(name.as_bytes())).expect("corpus source compiles");
        assert_eq!(
            execute(&program, true),
            execute(&program, false),
            "{}",
            path.display()
        );
    }
}
