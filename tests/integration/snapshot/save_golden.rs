//! Pins the complete debug-profile save stream, including compiler output,
//! registrations, engine metadata and cost-model version. Deterministic graph
//! reorders also change this pin, even when round-trip tests still pass.
//! Review the generated text companion before accepting any regeneration.
//!
//! Object ids are assigned at first encounter, so the fixture encodes the exact
//! depth-first preorder: for a table, each entry's key subtree then its value
//! subtree in storage order, and the metatable only after every entry; for a
//! closure, its bytecode and then its upvalues in vector order.
//!
//! To refresh deliberately after a reviewed save-output change, run
//! `brokkr run --debug --features snapshot regenerate_save_golden`. The
//! fixture must come from the profile the tests run under: a release build
//! can fold identical Rust functions (`unpack` and `table.unpack`), and the
//! save then writes the smallest id for both, so its bytes differ.

use std::path::PathBuf;

use dellingr::{DefaultCallbacks, State};

use save_golden_program::golden_bytes;

use crate::support::{self, save_golden_program};

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

    let golden_name = PathBuf::from(save_golden_program::GOLDEN_FIXTURE)
        .file_name()
        .expect("the golden fixture path has a file name")
        .to_string_lossy()
        .into_owned();
    let mut expected = vec![golden_name];
    expected.extend(
        RELEASED_FORMATS
            .iter()
            .map(|(version, _)| format!("save_released_v{version}.bin")),
    );
    expected.sort();
    assert_eq!(actual, expected, "every save fixture must be listed here");
}

// After a reviewed save-output change, regenerate the binary golden and its
// text companion with
// `brokkr run --debug --features snapshot regenerate_save_golden`.
#[test]
fn save_traversal_order_matches_golden_fixture() {
    let expected = std::fs::read(current_fixture_path()).expect(
        "golden fixture missing; run the explicit regeneration example with the snapshot feature",
    );
    let actual = golden_bytes();
    let text = std::fs::read_to_string(current_fixture_path().with_extension("txt"))
        .expect("text companion exists; run the save golden regenerator");
    assert_eq!(text, save_golden_program::golden_text(&expected));
    assert_eq!(
        actual.get(4..6),
        expected.get(4..6),
        "fixture version bytes must match the current serializer's FORMAT_VERSION"
    );
    assert_eq!(
        actual, expected,
        "save output changed: review traversal, compiler output, registrations, \
         engine metadata and cost-model changes before regenerating both fixtures"
    );
}

#[test]
fn golden_fixture_round_trips() {
    let bytes = std::fs::read(current_fixture_path()).expect("current fixture is present");
    let mut loaded = State::load_state(&bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("fixture loads");

    support::run(
        &mut loaded,
        r#"
            if iterator() ~= "b" then error("iterator position was not restored") end
            if empty_iterator() ~= "" then error("empty iterator was not restored") end
            sibling_write()
            if sibling_read() ~= 2 then error("shared upvalue cell was not restored") end
            result = captured()
                .. "|" .. tostring(cyclic.self == cyclic)
                .. "|" .. tostring(cyclic.other == keyed)
                .. "|" .. tostring(nested.inner.sibling == shared)
                .. "|" .. keyed[shared]
                .. "|" .. meta.tag
                .. "|" .. nested.inner.deep.leaf
                .. "|" .. ordered.extra
        "#,
    );
    loaded
        .get_global("result")
        .expect("result global exists after the probe runs");
    assert_eq!(
        loaded.to_string(-1).expect("probe returns a string"),
        "10|true|true|true|by-object-key|shared|leaf|tail"
    );
}

// Release sections and their release trees establish these versions:
// 0.4.0: 3170f83 (save_state.rs FORMAT_VERSION = 6),
// 0.5.0: 6c795d1 (save_state.rs FORMAT_VERSION = 7).
// Earlier releases predate the snapshot feature. Preserve these files and pins;
// on release, archive the current binary and add its version and fingerprint.
const RELEASED_FORMATS: &[(u16, u64)] = &[(6, 0x7ca2_be91_fa0f_7e03), (7, 0xcee0_80b7_9e3e_662b)];

/// The bytes after the magic, both version words and the engine label.
fn payload_after_header(bytes: &[u8]) -> &[u8] {
    let length = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("a save header carries a four-byte engine label length"),
    );
    &bytes[12 + length as usize..]
}

/// A save written by a released build is the compatibility evidence: while
/// its format and cost-model versions are still the current ones, the
/// current build must load it and write the same payload back. Compiler,
/// registration and golden-program changes leave the archived save alone, so
/// only a change in how saves are read or written trips this.
#[test]
fn released_formats_are_frozen() {
    let current = golden_bytes();
    for &(version, fingerprint) in RELEASED_FORMATS {
        let frozen = std::fs::read(fixture_path(&format!("save_released_v{version}.bin")))
            .expect("released format fixture must remain available");
        assert_eq!(u16::from_le_bytes([frozen[4], frozen[5]]), version);
        // FNV-1a is a review pin, not authentication of untrusted saves.
        let hash = frozen.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        });
        assert_eq!(hash, fingerprint, "released fixture changed");
        if frozen[4..8] != current[4..8] {
            continue;
        }
        let loaded = State::load_state(&frozen, Box::new(DefaultCallbacks), support::no_setup)
            .expect("a released save must load while its format and cost model are current");
        let resaved = loaded
            .save_state()
            .expect("a loaded released save re-saves")
            .bytes;
        assert_eq!(
            payload_after_header(&resaved),
            payload_after_header(&frozen),
            "the current build reads or writes released format {version} differently; \
             review compatibility and allocate a new format version"
        );
    }
}
