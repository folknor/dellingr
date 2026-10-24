//! Destinations and the write-if-changed policy shared by the
//! `regenerate_*` maintenance examples.

use std::path::{Path, PathBuf};

pub(crate) fn destination(tool: &str) -> PathBuf {
    // Host tool arguments select an output file, outside the deterministic VM.
    #[allow(clippy::disallowed_methods)]
    let mut args = std::env::args_os().skip(1);
    let output = args
        .next()
        .unwrap_or_else(|| panic!("usage: {tool} OUTPUT"));
    assert!(args.next().is_none(), "expected exactly one output path");
    PathBuf::from(output)
}

/// Write `bytes` to `output` only when its current contents differ, so an
/// unchanged regeneration leaves the file's timestamp alone, and report which.
pub(crate) fn write_changed(output: &Path, bytes: &[u8]) {
    let current = match std::fs::read(output) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("{} should be readable: {error}", output.display()),
    };
    if current.as_deref() == Some(bytes) {
        println!("unchanged {}", output.display());
    } else {
        std::fs::write(output, bytes)
            .unwrap_or_else(|error| panic!("{} should be writable: {error}", output.display()));
        println!("wrote {}", output.display());
    }
}
