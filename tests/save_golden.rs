#![cfg(feature = "snapshot")]

//! Pins the save codec's traversal order against a byte fixture captured
//! before the iterative-walker rewrite (notes/bugs.md #28).
//!
//! `saves_are_byte_stable` and `compiler_produced_save_resaves_byte_for_byte`
//! cannot catch a reordering: both run the *current* algorithm twice, so any
//! deterministic change in traversal order passes them while silently changing
//! every save file the crate produces. Only a fixture captured from the
//! previous implementation pins the order.
//!
//! Object ids are assigned at first encounter, so the fixture encodes the exact
//! depth-first preorder: for a table, each entry's key subtree then its value
//! subtree in storage order, and the metatable only after every entry; for a
//! closure, its bytecode and then its upvalues in vector order.
//!
//! To regenerate deliberately after an intentional format change:
//!     cargo test --features snapshot --test save_golden -- --ignored regenerate

use std::path::PathBuf;

use dellingr::{ArgCount, DefaultCallbacks, RetCount, State};

/// Exercises every edge kind the walker can take: several globals, an
/// object-valued key, an object-valued value, a shared reference reached twice,
/// a cycle, a metatable, a closure with multiple upvalues, and strings first
/// seen at different depths.
const GOLDEN_PROGRAM: &str = r#"
    shared = { tag = "shared" }

    keyed = {}
    keyed[shared] = "by-object-key"

    nested = { inner = { deep = { leaf = "leaf" } } }
    nested.inner.sibling = shared

    cyclic = {}
    cyclic.self = cyclic
    cyclic.other = keyed

    meta = setmetatable({ marked = true }, { __index = shared, name = "meta" })

    function make(a, b)
        local c = a + b
        return function() return a + b + c end
    end
    captured = make(2, 3)

    ordered = { 1, 2, 3, extra = "tail" }
    math.golden_delta = { marker = "environment delta" }
    golden_pointer = tostring(ordered)
"#;

fn golden_bytes() -> Vec<u8> {
    let mut state = State::new();
    state
        .load_string(GOLDEN_PROGRAM)
        .expect("golden program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("golden program runs");
    state.save_state().expect("golden state saves").bytes
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/save_golden.bin")
}

#[test]
#[ignore = "regeneration helper; run explicitly after an intentional format change"]
fn regenerate() {
    let path = fixture_path();
    std::fs::create_dir_all(path.parent().expect("fixture path has a parent"))
        .expect("fixture directory is writable");
    std::fs::write(&path, golden_bytes()).expect("fixture is writable");
    println!("wrote {}", path.display());
}

#[test]
fn save_traversal_order_matches_golden_fixture() {
    let expected = std::fs::read(fixture_path()).expect(
        "golden fixture missing; regenerate with \
         `cargo test --features snapshot --test save_golden -- --ignored regenerate`",
    );
    let actual = golden_bytes();
    assert_eq!(
        actual, expected,
        "save output changed. If this was an intentional format or traversal \
         change, regenerate the fixture; otherwise the walker reordered the \
         object graph and every existing save file is now unreadable."
    );
}

#[test]
fn golden_fixture_round_trips() {
    let bytes = std::fs::read(fixture_path()).expect("golden fixture present");
    let mut loaded =
        State::load_state(&bytes, Box::new(DefaultCallbacks), |_| {}).expect("fixture loads");

    loaded
        .load_string(
            r#"
            result = captured()
                .. "|" .. tostring(cyclic.self == cyclic)
                .. "|" .. tostring(cyclic.other == keyed)
                .. "|" .. tostring(nested.inner.sibling == shared)
                .. "|" .. keyed[shared]
                .. "|" .. meta.tag
                .. "|" .. nested.inner.deep.leaf
                .. "|" .. ordered.extra
        "#,
        )
        .expect("probe compiles");
    loaded
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("probe runs");
    loaded.get_global("result").unwrap();
    assert_eq!(
        loaded.to_string(-1).expect("probe returns a string"),
        "10|true|true|true|by-object-key|shared|leaf|tail"
    );
}
