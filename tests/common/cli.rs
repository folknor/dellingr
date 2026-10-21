//! Helpers for integration tests that invoke the CLI.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

/// A `Command` for the CLI binary Cargo built for this test run, for tests
/// that need to configure pipes themselves.
pub(crate) fn command() -> Command {
    Command::new(crate::common::dellingr_bin())
}

pub(crate) fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_from(Path::new("."), args)
}

pub(crate) fn run_from<I, S>(directory: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command()
        .current_dir(directory)
        .args(args)
        .output()
        .expect("dellingr CLI should run")
}
