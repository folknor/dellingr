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
fn cli_paths_and_test_file_mutations_stay_in_common_helpers() {
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
            assert_eq!(
                relative_path,
                Path::new("common/scratch.rs"),
                "test file mutations must use the scratch helper"
            );
        }
    }

    let scratch_source = fs::read_to_string(tests_dir.join("common/scratch.rs"))
        .expect("test scratch helper should be readable");
    assert!(
        scratch_source.contains("CARGO_TARGET_TMPDIR")
            && scratch_source.contains(".starts_with(scratch_root)"),
        "test file mutations must be restricted to Cargo's temporary test directory"
    );
}
