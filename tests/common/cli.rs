//! Helpers for integration tests that invoke the CLI.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

/// A `Command` for the CLI binary Cargo built for this test run, for tests
/// that need to configure pipes themselves.
pub(crate) fn command() -> Command {
    Command::new(crate::common::dellingr_bin())
}

/// A CLI command with the stress-test resource wrapper applied.
// Only the examples runner uses this; the CLI suites that include this file
// do not, and each test binary checks dead code on its own.
#[allow(dead_code)]
pub(crate) fn limited_command() -> Command {
    let mut command = Command::new("bash");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test_limited.sh"))
        .arg(crate::common::dellingr_bin());
    command
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
