// Integration tests include this file with `#[path]`, and `src/compiler.rs`
// compiles it into a unit test with `include!`, so it must stay
// self-contained: no inner attributes, no `crate::` or `super::` paths.
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the example Lua files in the same order as the shell differential gate.
///
/// Paths are absolute and rooted at the crate manifest. Directory and file
/// symlinks are skipped, matching `find -type f`, and ordering compares the
/// relative path bytes, matching `LC_ALL=C sort`.
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
