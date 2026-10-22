#![cfg(feature = "snapshot")]

//! Pins the save codec's traversal order against a byte fixture captured
//! before the iterative-walker rewrite.
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
//! To refresh deliberately after an intentional format change, run
//! `brokkr run --debug --features snapshot regenerate_save_golden`. The
//! fixture must come from the profile the tests run under: a release build
//! can fold identical Rust functions (`unpack` and `table.unpack`), and the
//! save then writes the smallest id for both, so its bytes differ.

use std::path::PathBuf;

use dellingr::{ArgCount, DefaultCallbacks, RetCount, State};

#[path = "common/save_golden_program.rs"]
mod save_golden_program;

use save_golden_program::golden_bytes;

const FIXTURE_NAMES: &[&str] = &["save_golden_current.bin"];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}"))
}

fn current_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(save_golden_program::GOLDEN_FIXTURE)
}

#[test]
fn every_save_fixture_is_declared_and_referenced() {
    let fixture_dir = fixture_path("");
    // Only save fixtures (.bin) are governed here; the directory also holds
    // other fixture kinds, such as the opcode encoding table.
    let mut actual = std::fs::read_dir(&fixture_dir)
        .expect("fixture directory exists")
        .map(|entry| {
            entry
                .expect("fixture directory entry is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".bin"))
        .collect::<Vec<_>>();
    actual.sort();

    let mut expected = FIXTURE_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected, "every save fixture must be listed here");
}

// After an intentional serializer format change, regenerate with
// `brokkr run --debug --features snapshot regenerate_save_golden`.
#[test]
fn save_traversal_order_matches_golden_fixture() {
    let expected = std::fs::read(current_fixture_path()).expect(
        "golden fixture missing; run the explicit regeneration example with the snapshot feature",
    );
    let actual = golden_bytes();
    assert_eq!(
        actual.get(4..6),
        expected.get(4..6),
        "fixture version bytes must match the current serializer's FORMAT_VERSION"
    );
    assert_eq!(
        actual, expected,
        "save output changed. If this was an intentional format or traversal \
         change, regenerate the fixture; otherwise the walker reordered the \
         object graph and every existing save file is now unreadable."
    );
}

#[test]
fn golden_fixture_round_trips() {
    let bytes = std::fs::read(current_fixture_path()).expect("current fixture is present");
    let mut loaded = State::load_state(&bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("fixture loads");

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
    loaded
        .get_global("result")
        .expect("result global exists after the probe runs");
    assert_eq!(
        loaded.to_string(-1).expect("probe returns a string"),
        "10|true|true|true|by-object-key|shared|leaf|tail"
    );
}
