//! Scratch files and directories for integration tests.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// process-global: only makes names unique; a test sees its own value only.
static NEXT_SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

fn scratch_root() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

fn next_scratch_path(name: &str) -> PathBuf {
    let unique_id = NEXT_SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
    scratch_root().join(format!(
        "dellingr-cli-{name}-{}-{unique_id}",
        std::process::id()
    ))
}

/// A per-test scratch directory that skips paths left by killed test processes.
pub(crate) struct ScratchDirectory {
    path: PathBuf,
}

impl ScratchDirectory {
    pub(crate) fn new(name: &str) -> Self {
        loop {
            let path = next_scratch_path(name);
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("test directory should be created: {error}"),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir(&self.path)
            && error.kind() != io::ErrorKind::NotFound
            && !std::thread::panicking()
        {
            panic!("test directory should be removed: {error}");
        }
    }
}

/// A Lua script stored under Cargo's temporary test directory.
pub(crate) struct ScratchScript {
    path: PathBuf,
    _directory: Option<ScratchDirectory>,
}

impl ScratchScript {
    pub(crate) fn new(name: &str, source: impl AsRef<[u8]>) -> Self {
        let directory = ScratchDirectory::new(name);
        let path = directory.path().join("script.lua");
        let mut script = Self::at(path, source);
        script._directory = Some(directory);
        script
    }

    pub(crate) fn at(path: PathBuf, source: impl AsRef<[u8]>) -> Self {
        write_scratch_file(&path, source.as_ref()).expect("test script should be written");
        Self {
            path,
            _directory: None,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchScript {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
            && !std::thread::panicking()
        {
            panic!("test script should be removed: {error}");
        }
    }
}

fn write_scratch_file(path: &Path, source: &[u8]) -> io::Result<()> {
    let scratch_root = fs::canonicalize(scratch_root())?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "scratch file has no parent"))?;
    if !fs::canonicalize(parent)?.starts_with(scratch_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test scratch files must stay under CARGO_TARGET_TMPDIR",
        ));
    }

    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(source) {
        drop(file);
        if let Err(remove_error) = fs::remove_file(path)
            && remove_error.kind() != io::ErrorKind::NotFound
        {
            return Err(remove_error);
        }
        return Err(error);
    }
    Ok(())
}

/// Write the on-demand boundary A/B evidence to its spec-mandated location.
/// This fixed path is the only exception to the ephemeral test-file root:
/// the ignored dump must outlive the test process for external comparison.
/// Compiled under exactly the configuration of its one caller,
/// `error_boundary_transcript`.
#[cfg(all(
    debug_assertions,
    not(any(
        feature = "snapshot",
        feature = "fault-injection",
        feature = "hotpath",
        feature = "hotpath-alloc"
    ))
))]
pub(crate) fn write_boundary_transcript(bytes: &[u8]) -> io::Result<()> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(".scratch/error-boundary");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("transcript.txt"), bytes)
}
