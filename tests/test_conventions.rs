use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

// The gate test exercises only part of each helper module.
#[allow(dead_code)]
#[path = "common/save_fixture.rs"]
mod save_fixture;
#[allow(dead_code)]
#[path = "common/scratch.rs"]
mod scratch;

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("test source directory should be readable") {
        let path = entry.expect("test source entry should be readable").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn cli_and_file_mutation_helpers_stay_in_tests_common() {
    let tests_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources = Vec::new();
    collect_rust_files(&tests_dir, &mut sources);

    let cli_binary_marker = concat!("CARGO", "_BIN_EXE_");
    let direct_write_marker = concat!("fs::", "write(");
    let write_all_marker = concat!("write", "_all(");
    let file_create_marker = concat!("File::", "create(");
    let file_open_marker = concat!("OpenOptions::", "new(");
    let directory_create_marker = concat!("fs::", "create_dir(");
    let directory_create_all_marker = concat!("fs::", "create_dir_all(");
    let file_remove_marker = concat!("fs::", "remove_file(");
    let directory_remove_marker = concat!("fs::", "remove_dir(");

    for source_path in &sources {
        let source =
            fs::read_to_string(source_path).expect("Rust test source should be valid UTF-8");
        let relative_path = source_path
            .strip_prefix(&tests_dir)
            .expect("test source should be under tests/");

        if source.contains(cli_binary_marker) {
            assert_eq!(
                relative_path,
                Path::new("common/mod.rs"),
                "the Cargo-built CLI binary path belongs in tests/common/mod.rs"
            );
        }

        if source.contains(direct_write_marker) {
            assert_eq!(
                relative_path,
                Path::new("common/save_fixture.rs"),
                "checked-in fixture writes belong in the explicitly gated helper"
            );
        }

        let has_file_mutation = [
            direct_write_marker,
            write_all_marker,
            file_create_marker,
            file_open_marker,
            directory_create_marker,
            directory_create_all_marker,
            file_remove_marker,
            directory_remove_marker,
        ]
        .iter()
        .any(|marker| source.contains(*marker));
        if has_file_mutation {
            assert!(
                relative_path.starts_with("common"),
                "test file mutations belong in tests/common: {}",
                relative_path.display()
            );
        }
    }
}

#[test]
fn save_fixture_regeneration_requires_the_exact_opt_in_before_writing() {
    let expected_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/save_golden_current.bin");
    assert_eq!(
        save_fixture::save_golden_fixture_path(),
        expected_fixture,
        "the regeneration helper must target the checked-in golden"
    );

    let directory = scratch::ScratchDirectory::new("fixture-regeneration-gate");
    let missing_script =
        scratch::ScratchScript::at(directory.path().join("missing-golden.bin"), "placeholder");
    let missing_fixture = missing_script.path().to_owned();
    drop(missing_script);
    let fixture_script =
        scratch::ScratchScript::at(directory.path().join("save-golden.bin"), "original bytes");
    let fixture = fixture_script.path().to_owned();

    for authorization in [None, Some(OsStr::new("0")), Some(OsStr::new("true"))] {
        let result = save_fixture::rewrite_save_golden_fixture_at(
            b"fixture bytes",
            &missing_fixture,
            authorization,
        );
        assert!(result.is_err(), "an invalid opt-in must be rejected");
        assert!(
            !missing_fixture.exists(),
            "an invalid opt-in must not create a fixture"
        );
    }

    for authorization in [None, Some(OsStr::new("0")), Some(OsStr::new("true"))] {
        let result =
            save_fixture::rewrite_save_golden_fixture_at(b"fixture bytes", &fixture, authorization);
        assert!(result.is_err(), "an invalid opt-in must be rejected");
        assert_eq!(
            fs::read(&fixture).expect("existing fixture should be readable"),
            b"original bytes",
            "an invalid opt-in must not overwrite a fixture"
        );
    }

    save_fixture::rewrite_save_golden_fixture_at(b"fixture bytes", &fixture, Some(OsStr::new("1")))
        .expect("the exact regeneration opt-in should authorize a fixture write");
    assert_eq!(
        fs::read(&fixture).expect("authorized fixture should be readable"),
        b"fixture bytes"
    );
}
