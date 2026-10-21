use std::fs;
use std::path::{Path, PathBuf};

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

    let fixture_helper = fs::read_to_string(tests_dir.join("common/save_fixture.rs"))
        .expect("save fixture helper should be present");
    assert!(fixture_helper.contains("match std::env::var_os(\"DELLINGR_REGENERATE_GOLDEN\")"));
    assert!(fixture_helper.contains("Some(value) if value == OsStr::new(\"1\") => {}"));
    assert!(fixture_helper.contains("tests/fixtures/save_golden_current.bin"));
}
