// Integration tests include this file with `#[path]`, and `src/lib.rs`
// compiles it into the unit tests with `include!`, so it must stay
// self-contained: no inner attributes, no `crate::` or `super::` paths.
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the example Lua files, in a host-independent order.
///
/// Paths are absolute and rooted at the crate manifest. Directory and file
/// symlinks are skipped, as `find -type f` skips them, and ordering compares
/// the relative path bytes, as `LC_ALL=C sort` does (`scripts/lib.sh` lists
/// the corpus the same way for the manual `scripts/diff_test.sh`).
pub(crate) fn example_corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut paths = Vec::new();
    collect_lua_files(&root, &mut paths);
    paths.sort_by_cached_key(|path| {
        path.strip_prefix(&root)
            .expect("collected example paths must be under the examples directory")
            .as_os_str()
            .as_encoded_bytes()
            .to_vec()
    });
    paths
}

fn collect_lua_files(directory: &Path, paths: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("examples directory must be readable");
    for entry in entries {
        let entry = entry.expect("example directory entry must be readable");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("example directory entry type must be readable");
        if file_type.is_dir() {
            collect_lua_files(&path, paths);
        } else if file_type.is_file()
            && path.extension().is_some_and(|extension| extension == "lua")
        {
            paths.push(path);
        }
    }
}
