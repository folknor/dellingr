//! Every example saved after its setup, restored, and continued on both
//! states, one test per example (`examples::<name>`), expanded from the
//! checked-in list `tests/common/examples.rs`. Corpus snapshots are compared
//! only within this process and build. In particular, Rust function folding
//! can change saved ids between profiles.

use dellingr::{ArgCount, LuaType, Program, RetCount, State};

use crate::support::{self, Recorder};

/// One round trip per listed example.
mod examples {
    macro_rules! examples {
        ($($name:ident: $path:literal,)*) => {
            $(
                #[test]
                fn $name() {
                    super::round_trip($path, concat!(env!("CARGO_MANIFEST_DIR"), "/", $path));
                }
            )*
        };
    }

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/common/examples.rs"
    ));
}

fn bounded_source(source: &str) -> String {
    // The kernels' standalone timing footers repeat a single call. Run that
    // call once during setup; continuation then calls it once on each branch.
    // Leave all other source intact, including the sort correctness footer.
    source
        .lines()
        .map(|line| {
            if let Some(count) = line
                .strip_prefix("for i = 1, ")
                .and_then(|tail| tail.strip_suffix(" do _bench() end"))
                && !count.is_empty()
                && count.bytes().all(|byte| byte.is_ascii_digit())
            {
                "_bench()"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `name` is the example's repository-relative path, its chunk name.
fn round_trip(name: &str, path: &str) {
    let source = std::fs::read_to_string(path).expect("example source is readable");
    let program = Program::compile(bounded_source(&source), Some(name.as_bytes()))
        .unwrap_or_else(|error| panic!("{name}: compile: {error}"));
    let (mut original, original_output) = Recorder::state();
    original
        .load(&program)
        .unwrap_or_else(|error| panic!("{name}: load: {error}"));
    original
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_or_else(|error| panic!("{name}: setup: {error}"));
    assert_eq!(original.get_top(), 0, "{name}: setup stack");
    assert!(
        !original_output
            .prints()
            .iter()
            .any(|line| line.ends_with(b": false")),
        "{name}: example reported failure"
    );
    let saved = original
        .save_state()
        .unwrap_or_else(|error| panic!("{name}: save: {error}"));
    let loaded_output = Recorder::default();
    let mut loaded = State::load_state(
        &saved.bytes,
        Box::new(loaded_output.clone()),
        support::no_setup,
    )
    .unwrap_or_else(|error| panic!("{name}: restore: {error}"));
    assert_eq!(loaded.get_top(), 0, "{name}: restored stack");
    assert_eq!(
        original.cost_used(),
        loaded.cost_used(),
        "{name}: restored cost"
    );
    assert_eq!(
        saved.bytes,
        loaded.save_state().expect("restored state saves").bytes,
        "{name}: immediate resave"
    );
    original_output.clear();
    loaded_output.clear();

    for state in [&mut original, &mut loaded] {
        state
            .get_global("_bench")
            .expect("global lookup fits the empty stack");
        if state.typ(-1) == LuaType::Nil {
            state.pop(1).expect("global lookup pushed one value");
        } else {
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .unwrap_or_else(|error| panic!("{name}: continuation: {error}"));
        }
    }
    assert_eq!(
        original_output.prints(),
        loaded_output.prints(),
        "{name}: output"
    );
    assert_eq!(
        original_output.errors(),
        loaded_output.errors(),
        "{name}: errors"
    );
    assert_eq!(
        original.cost_used(),
        loaded.cost_used(),
        "{name}: continued cost"
    );
    assert_eq!(
        original
            .save_state()
            .expect("original continuation saves")
            .bytes,
        loaded
            .save_state()
            .expect("loaded continuation saves")
            .bytes,
        "{name}: continued save"
    );
}
