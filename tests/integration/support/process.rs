//! Helpers for integration tests that run the CLI binary as a process. (Not
//! named `cli`: no module may share a subject's name, see
//! `corpus::test_conventions`.)

use std::ffi::OsStr;
#[cfg(unix)]
use std::io;
use std::path::Path;
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::OnceLock;

#[cfg(unix)]
const SCRIPT_LIMITS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/lib.sh"));

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

/// A CLI command with the stress-test address-space cap applied on Unix.
pub(crate) fn limited_command() -> Command {
    let mut command = Command::new(super::dellingr_bin());
    command.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let limit = address_space_limit();
        let limits = libc::rlimit {
            rlim_cur: limit,
            rlim_max: limit,
        };

        // SAFETY: The hook only invokes setrlimit with a valid captured value
        // and reports its OS error. It does not allocate, acquire locks, or
        // access mutable process state after fork.
        unsafe {
            command.pre_exec(move || {
                // `limits` is valid for the duration of this system call, and
                // setrlimit reads it synchronously.
                if libc::setrlimit(libc::RLIMIT_AS, &limits) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
    }

    command
}

#[cfg(unix)]
fn address_space_limit() -> libc::rlim_t {
    // process-global: cache the source limit and inherited hard cap for this test process.
    static LIMIT_BYTES: OnceLock<libc::rlim_t> = OnceLock::new();

    *LIMIT_BYTES.get_or_init(|| {
        let limit_kb = SCRIPT_LIMITS
            .lines()
            .find_map(|line| line.strip_prefix("readonly TEST_LIMIT_KB="))
            .unwrap_or_else(|| panic!("scripts/lib.sh must define TEST_LIMIT_KB"));
        let limit_kb = match limit_kb.parse::<u64>() {
            Ok(limit_kb) => limit_kb,
            Err(_) => panic!("scripts/lib.sh TEST_LIMIT_KB must be an unsigned integer"),
        };
        let limit_bytes = match limit_kb.checked_mul(1024) {
            Some(limit_bytes) => limit_bytes,
            None => panic!("scripts/lib.sh TEST_LIMIT_KB is too large"),
        };
        let limit_bytes = match libc::rlim_t::try_from(limit_bytes) {
            Ok(limit_bytes) => limit_bytes,
            Err(_) => panic!("scripts/lib.sh TEST_LIMIT_KB does not fit in libc::rlim_t"),
        };
        let mut inherited_limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };

        // SAFETY: `inherited_limits` is a valid writable rlimit for this call.
        if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut inherited_limits) } != 0 {
            panic!(
                "could not read inherited address-space limit: {}",
                io::Error::last_os_error()
            );
        }

        limit_bytes.min(inherited_limits.rlim_max)
    })
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
