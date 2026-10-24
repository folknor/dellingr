//! Write the corpus cost golden (`tests/common/corpus_cost_golden.rs`) to an explicit
//! destination: every listed example's `cost_used` from one run on a fresh
//! State, as the CLI runs it (same callbacks contract, chunk name and call, no
//! budget). Refuses to write a fixture that records a new cost for unchanged
//! source under an unchanged `COST_MODEL_VERSION`.

#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../tests/common/corpus_cost_golden.rs"]
mod corpus_cost;
#[cfg(not(test))]
#[path = "tools/output.rs"]
mod output;

#[cfg(not(test))]
mod corpus {
    pub(crate) struct Example {
        pub(crate) relative: &'static str,
        pub(crate) path: &'static str,
    }

    macro_rules! examples {
        ($($name:ident: $path:literal,)*) => {
            pub(crate) const EXAMPLES: &[Example] = &[$(Example {
                relative: $path,
                path: concat!(env!("CARGO_MANIFEST_DIR"), "/", $path),
            },)*];
        };
    }

    include!("../tests/common/examples.rs");
}

/// Prints are discarded; the cost of producing them is charged before the
/// callback sees them.
#[cfg(not(test))]
struct Discard;

#[cfg(not(test))]
impl dellingr::HostCallbacks for Discard {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}
}

/// The `cost_used` of one CLI-equivalent run. The corpus gate passes the
/// absolute path to the CLI, which uses it as the chunk name.
#[cfg(not(test))]
fn cost_of(example: &corpus::Example) -> u64 {
    use dellingr::{ArgCount, Program, RetCount, State};

    let source = std::fs::read(example.path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", example.relative));
    let program = Program::compile(&source, Some(example.path.as_bytes()))
        .unwrap_or_else(|error| panic!("{}: {error}", example.relative));
    let mut state = State::with_callbacks(Box::new(Discard));
    state
        .load(&program)
        .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)))
        .unwrap_or_else(|error| panic!("{}: {error}", example.relative));
    state.cost_used()
}

#[cfg(not(test))]
fn main() {
    let output = output::destination("regenerate_corpus_cost");
    let replacement = corpus_cost::Fixture {
        cost_model: dellingr::COST_MODEL_VERSION,
        rows: corpus::EXAMPLES
            .iter()
            .map(|example| corpus_cost::Row {
                path: example.relative.to_owned(),
                source: corpus_cost::source_digest(&std::fs::read(example.path).unwrap_or_else(
                    |error| panic!("{} should be readable: {error}", example.relative),
                )),
                cost: cost_of(example),
            })
            .collect(),
    };
    match std::fs::read_to_string(&output) {
        Ok(current) => {
            let current = corpus_cost::parse(&current)
                .unwrap_or_else(|error| panic!("{}: {error}", output.display()));
            let drift = current.unversioned_changes(&replacement);
            assert!(
                drift.is_empty(),
                "refusing to record new costs for unchanged source under cost model {}; bump \
                 COST_MODEL_VERSION and document it in src/cost_meter.rs first:\n{}",
                replacement.cost_model,
                drift.join("\n")
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("{} should be readable: {error}", output.display()),
    }
    output::write_changed(&output, corpus_cost::render(&replacement).as_bytes());
}

#[cfg(test)]
fn main() {}
