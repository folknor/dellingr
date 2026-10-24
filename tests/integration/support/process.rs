//! Helpers for integration tests that run the CLI binary as a process. (Not
//! named `cli`: no module may share a subject's name, see
//! `corpus::test_conventions`.)

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

// The CLI's output prefixes, as `src/main.rs` prints them.
/// Prefix of an error the CLI prints.
pub(crate) const ERROR_PREFIX: &str = "Error: ";
/// Prefix of the CLI's cost summary line.
pub(crate) const COST_USED_PREFIX: &str = "Cost used: ";

/// A `Command` for the CLI binary Cargo built for this test run, for tests
/// that need to configure pipes themselves.
pub(crate) fn command() -> Command {
    Command::new(super::dellingr_bin())
}

/// A CLI command with the stress-test resource wrapper applied.
pub(crate) fn limited_command() -> Command {
    let mut command = Command::new("bash");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test_limited.sh"))
        .arg(super::dellingr_bin())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")));
    command
}

pub(crate) fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_from(Path::new(env!("CARGO_MANIFEST_DIR")), args)
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
