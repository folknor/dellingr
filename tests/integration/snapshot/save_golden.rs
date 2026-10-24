//! Pins the save payload, including compiler output, standard-library
//! function ids and the cost the golden program is charged. Compatibility
//! metadata is checked separately. Deterministic graph reorders also change
//! this pin, even when round-trip tests still pass. Review the generated text
//! companion before accepting any regeneration.
//!
//! Object ids are assigned at first encounter, so the fixture encodes the exact
//! depth-first preorder: for a table, each entry's key subtree then its value
//! subtree in storage order, and the metatable only after every entry; for a
//! closure, its bytecode and then its upvalues in vector order.
//!
//! The payload is the same in every build profile: a saved Rust function is
//! its function table entry's id, never its code address, so identical-code
//! folding in an optimized build cannot reach the bytes. brokkr.toml runs this
//! module in a release sweep as well as the dev ones to keep it that way.
//!
//! To refresh deliberately after a reviewed save-output change, run
//! `brokkr run --features snapshot regenerate_save_golden` (any profile).

use std::path::PathBuf;

use dellingr::{DefaultCallbacks, State};

use save_golden_program::{golden_bytes, payload_after_header};

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
            .map(|archive| archive.file.to_owned()),
    );
    expected.sort();
    assert_eq!(actual, expected, "every save fixture must be listed here");
}

// After a reviewed save-output change, regenerate the binary golden and its
// text companion with
// `brokkr run --features snapshot regenerate_save_golden`.
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
        payload_after_header(&actual),
        payload_after_header(&expected),
        "save output changed. Any of these changes it: the save format or \
         traversal order, compiler output, standard-library function ids, the \
         golden program, or the charges the program incurs (a cost-model change, \
         whether or not it bumps COST_MODEL_VERSION). Saved bytes are \
         replay-visible: review the text companion's diff and call the change out \
         before regenerating both fixtures. A traversal reorder alone does not \
         make old saves unreadable; the released-format check below decides \
         compatibility."
    );
}

#[test]
fn golden_fixture_round_trips() {
    let mut bytes = std::fs::read(current_fixture_path()).expect("current fixture is present");
    // The current golden pins payload, not compatibility metadata. The load
    // probe exercises that payload under the current cost model; archived
    // releases below retain their original headers and strict admission.
    bytes[6..8].copy_from_slice(&dellingr::COST_MODEL_VERSION.to_le_bytes());
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
// on release, regenerate the current binary at the release commit, archive it
// under its format and cost-model pair and add both words and its fingerprint
// (docs/save-state.md "Format release policy and golden review").
struct ReleasedArchive {
    format: u16,
    cost_model: u16,
    file: &'static str,
    fingerprint: u64,
}

const RELEASED_FORMATS: &[ReleasedArchive] = &[
    ReleasedArchive {
        format: 6,
        cost_model: 2,
        file: "save_released_v6.bin",
        fingerprint: 0x7ca2_be91_fa0f_7e03,
    },
    ReleasedArchive {
        format: 7,
        cost_model: 5,
        file: "save_released_v7.bin",
        fingerprint: 0xcee0_80b7_9e3e_662b,
    },
];

/// A save written by a released build is the compatibility evidence: while
/// its format and cost-model versions are still the current ones, the
/// current build must load it and write the same payload back. Compiler,
/// registration and golden-program changes leave the archived save alone, so
/// only a change in how saves are read or written trips this.
#[test]
fn released_formats_are_frozen() {
    let current = golden_bytes();
    for (index, archive) in RELEASED_FORMATS.iter().enumerate() {
        assert!(
            RELEASED_FORMATS[..index]
                .iter()
                .all(|previous| (previous.format, previous.cost_model)
                    != (archive.format, archive.cost_model)),
            "archive keys must be unique format/cost-model pairs"
        );
        let version = archive.format;
        let frozen = std::fs::read(fixture_path(archive.file))
            .expect("released format fixture must remain available");
        assert_eq!(u16::from_le_bytes([frozen[4], frozen[5]]), version);
        assert_eq!(
            u16::from_le_bytes([frozen[6], frozen[7]]),
            archive.cost_model
        );
        // FNV-1a is a review pin, not authentication of untrusted saves.
        let hash = frozen.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        });
        assert_eq!(hash, archive.fingerprint, "released fixture changed");
        if frozen[4..8] != current[4..8] {
            eprintln!(
                "historical archive {}: fingerprint checked; load requires matching format and cost model",
                archive.file
            );
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
