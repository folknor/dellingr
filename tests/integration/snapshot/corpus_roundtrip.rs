//! Corpus snapshots are compared only within this process and build. In
//! particular, Rust function folding can change saved ids between profiles.

use std::path::Path;

use dellingr::{ArgCount, LuaType, Program, RetCount, State};

use crate::support::{self, Recorder, examples};

// These standalone workloads spend their time in stress loops or parsing a
// generated source, rather than adding distinct snapshot continuation cases.
// Keep the exclusions explicit so new examples enter the round trip by default.
const SKIPPED: &[&str] = &[
    "stress_tables.lua",
    "stress_gc_stack.lua",
    "stress_rustfn.lua",
    "upvalue_stress.lua",
    "parse/large_source.lua",
];

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

fn round_trip(path: &Path) {
    let name = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .expect("the shared example list is manifest-rooted")
        .to_string_lossy();
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

fn bucket(index: usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    // Separate tests bound work under the per-test dev-sweep timeout, without
    // depending on worker parallelism. Every shared-list entry gets a bucket.
    let paths = examples::example_corpus();
    assert!(!paths.is_empty(), "the example corpus must not be empty");
    for path in paths
        .iter()
        .enumerate()
        .filter_map(|(position, path)| (position % 8 == index).then_some(path))
    {
        let relative = path.strip_prefix(&root).expect("example is under examples");
        if !SKIPPED.iter().any(|skip| relative == Path::new(skip)) {
            round_trip(path);
        }
    }
}

#[test]
fn corpus_roundtrip_0() {
    bucket(0);
}
#[test]
fn corpus_roundtrip_1() {
    bucket(1);
}
#[test]
fn corpus_roundtrip_2() {
    bucket(2);
}
#[test]
fn corpus_roundtrip_3() {
    bucket(3);
}
#[test]
fn corpus_roundtrip_4() {
    bucket(4);
}
#[test]
fn corpus_roundtrip_5() {
    bucket(5);
}
#[test]
fn corpus_roundtrip_6() {
    bucket(6);
}
#[test]
fn corpus_roundtrip_7() {
    bucket(7);
}
