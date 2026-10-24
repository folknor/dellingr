//! Every process the integration tests start is constructed here: the CLI
//! binary Cargo built for this test run, and the reference Lua interpreters
//! the example corpus is compared against. A `brokkr.toml` textlint rule keeps
//! `Command` construction out of every other Rust file. (Not named `cli`: no
//! module may share a subject's name, see `corpus::test_conventions`.)

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(unix)]
const SCRIPT_LIMITS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/lib.sh"));

// The CLI's output prefixes, as `src/main.rs` prints them.
/// Prefix of an error the CLI prints.
pub(crate) const ERROR_PREFIX: &str = "Error: ";
/// Prefix of the CLI's cost summary line.
pub(crate) const COST_USED_PREFIX: &str = "Cost used: ";

/// The probe's deadline. It prints one line, so this only has to outlast a
/// loaded host; it is well inside brokkr's per-test cap, so a hung probe fails
/// the test that ran it instead of stopping the run.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);
/// The probe's output limit, far above the one line it prints.
const PROBE_OUTPUT_LIMIT: usize = 1024;

/// Asks the CLI which snapshot shape it was built with and refuses a binary
/// whose shape differs from this test harness (see `super::dellingr_bin`).
/// The command is built here rather than through `command()`, which would
/// re-enter `dellingr_bin` while it initialises.
pub(super) fn probe_snapshot_shape(binary: &str) -> Result<(), String> {
    let mut command = Command::new(binary);
    command.arg("--internal-snapshot-shape");
    let run = capture(
        &mut command,
        PROBE_OUTPUT_LIMIT,
        Deadline::after(PROBE_DEADLINE),
    )
    .map_err(|error| format!("CLI snapshot shape probe: {error}"))?;
    let status = match run.end {
        End::Exited(status) => status,
        End::Cut(Cut::OutputExceeded) => {
            return Err(format!(
                "CLI snapshot shape probe printed more than {PROBE_OUTPUT_LIMIT} bytes: {:?}",
                String::from_utf8_lossy(&run.stdout)
            ));
        }
        End::Cut(Cut::Unfinished | Cut::CaptureUnfinished(_)) => {
            return Err(format!(
                "CLI snapshot shape probe did not finish within {}s",
                PROBE_DEADLINE.as_secs()
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    if !status.success() || !run.stderr.is_empty() {
        return Err(format!(
            "CLI snapshot shape probe failed ({status}): stdout {stdout:?}, stderr {stderr:?}"
        ));
    }
    let expected = if cfg!(feature = "snapshot") {
        "snapshot=1\n"
    } else {
        "snapshot=0\n"
    };
    if stdout != expected {
        return Err(format!(
            "Cargo-built CLI snapshot shape differs from the test harness ({stdout:?}, \
             expected {expected:?}); rebuild this shape"
        ));
    }
    Ok(())
}

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
    apply_limits(&mut command, None);
    command
}

/// A reference interpreter the example corpus is compared against.
#[cfg(not(feature = "hotpath"))]
#[derive(Clone, Copy)]
pub(crate) enum ReferenceLua {
    Lua52,
    Lua54,
}

#[cfg(not(feature = "hotpath"))]
impl ReferenceLua {
    /// Every reference, in the order the corpus runs them.
    pub(crate) const ALL: [Self; 2] = [Self::Lua52, Self::Lua54];

    /// The executable, resolved from `PATH`. `scripts/check_tools.py` pins
    /// the version each name must report.
    pub(crate) fn program(self) -> &'static str {
        match self {
            Self::Lua52 => "lua5.2",
            Self::Lua54 => "lua5.4",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Lua52 => "Lua 5.2",
            Self::Lua54 => "Lua 5.4",
        }
    }
}

/// A command running `script` under the reference interpreter `lua`.
///
/// The reference runs from the manifest directory, as the CLI does, under
/// `LC_ALL=C` (dellingr's character classes are fixed ASCII, while reference
/// Lua follows the locale) and with `-E`, so `LUA_INIT` and `LUA_PATH` cannot
/// change what it does. On Unix it gets the CLI's address-space cap and
/// `RLIMIT_NPROC` 0: it cannot create a process, so `os.execute` and
/// `io.popen` fail, and a script cannot leave a descendant behind that holds
/// its output pipes or outlives the test. (The CLI needs no such limit: it has
/// no `os` or `io` library, so it cannot start a process at all.) The process
/// limit does not bind a process running as uid 0, so a root run is refused
/// rather than left uncontained. A non-root process with CAP_SYS_RESOURCE or
/// CAP_SYS_ADMIN in its ambient set is the one configuration that would still
/// escape it; it is unsupported.
#[cfg(not(feature = "hotpath"))]
pub(crate) fn reference_command(lua: ReferenceLua, script: &Path) -> Result<Command, String> {
    #[cfg(unix)]
    {
        // SAFETY: getuid and geteuid take no arguments, cannot fail and touch
        // no memory the caller owns.
        let privileged = unsafe { libc::getuid() == 0 || libc::geteuid() == 0 };
        if privileged {
            return Err(format!(
                "refusing to run {} as uid 0: RLIMIT_NPROC cannot stop it starting processes",
                lua.label()
            ));
        }
    }
    let mut command = Command::new(lua.program());
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .env("LC_ALL", "C")
        .arg("-E")
        .arg(script);
    #[cfg(unix)]
    apply_limits(&mut command, Some(0));
    Ok(command)
}

/// Applies the address-space cap and, when `processes` is given, the
/// `RLIMIT_NPROC` limit to the child before it executes.
#[cfg(unix)]
fn apply_limits(command: &mut Command, processes: Option<libc::rlim_t>) {
    use std::os::unix::process::CommandExt;

    let address_space = address_space_limit();
    // SAFETY: The hook only invokes setrlimit with valid captured values and
    // reports its OS error. It does not allocate, acquire locks, or access
    // mutable process state after fork.
    unsafe {
        command.pre_exec(move || {
            let limit = |value| libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            // Each rlimit is valid for the duration of its system call, and
            // setrlimit reads it synchronously.
            if libc::setrlimit(libc::RLIMIT_AS, &limit(address_space)) != 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(processes) = processes
                && libc::setrlimit(libc::RLIMIT_NPROC, &limit(processes)) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
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

/// A point in time the processes of a test must finish by.
///
/// The harness reads the host clock only to bound its own child processes,
/// which affects no Lua-visible value.
#[allow(
    clippy::disallowed_types,
    reason = "Bounding test processes needs the host clock; no Lua-visible value depends on it."
)]
#[derive(Clone, Copy)]
pub(crate) struct Deadline(std::time::Instant);

#[allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Bounding test processes needs the host clock; no Lua-visible value depends on it."
)]
impl Deadline {
    /// The deadline `budget` from now.
    pub(crate) fn after(budget: Duration) -> Self {
        Self(std::time::Instant::now() + budget)
    }

    /// The time left, or `None` once the deadline has passed.
    pub(crate) fn remaining(self) -> Option<Duration> {
        self.0
            .checked_duration_since(std::time::Instant::now())
            .filter(|left| !left.is_zero())
    }

    pub(crate) fn passed(self) -> bool {
        self.remaining().is_none()
    }
}

/// How a captured process ended.
pub(crate) enum End {
    /// It exited and both output streams were read to their end.
    Exited(ExitStatus),
    /// The capture was cut short; the process has been reaped.
    Cut(Cut),
}

/// Why a capture was cut short.
pub(crate) enum Cut {
    /// Its combined stdout and stderr passed the output limit; it was killed.
    OutputExceeded,
    /// It had not exited by the deadline; it was killed.
    Unfinished,
    /// It exited, but its output had not been read to the end by the
    /// deadline.
    CaptureUnfinished(ExitStatus),
}

/// A process's end and the output read from it (everything, after a clean
/// exit; what had arrived, after a cut).
pub(crate) struct Captured {
    pub(crate) end: End,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// The polling interval of the one wait that has no event to block on: both
/// output streams have closed, which normally means the process has exited,
/// and only the exit status is not visible yet. A process that closed both
/// streams and kept running is polled at this interval until the deadline.
const EXIT_POLL: Duration = Duration::from_millis(1);

enum ReaderEvent {
    /// The combined output just passed the limit.
    Exceeded,
    /// The reader of stream `0` (stdout) or `1` (stderr) reached the end of
    /// its pipe, or failed to read it. Each reader sends this exactly once,
    /// as its last act.
    Finished(usize, io::Result<()>),
}

const STREAM_NAMES: [&str; 2] = ["stdout", "stderr"];

/// Runs `command` with stdin closed and stdout and stderr captured separately,
/// until it exits, its combined output passes `output_limit` bytes, or
/// `deadline` passes; in the last two cases it is killed. The child is reaped
/// on every path. An `Err` is an operational failure: the deadline had
/// already passed (nothing is started), the process could not be started, its
/// output could not be read, or its wait failed.
///
/// Waiting blocks on the output readers' events, so a process that exits
/// wakes the wait at once. Containment is the direct child: the CLI cannot
/// start processes and the references are forbidden to (see
/// `reference_command`), so a process's exit closes its pipes. A reader is
/// joined only once it has reported its end; one still blocked when the
/// capture is cut is detached, so a containment failure can leak a thread but
/// cannot hang the test. The deadline bounds normal execution and output
/// capture; a process the kernel keeps from dying after SIGKILL is left to
/// brokkr's per-test watchdog.
pub(crate) fn capture(
    command: &mut Command,
    output_limit: usize,
    deadline: Deadline,
) -> Result<Captured, String> {
    // Building the command can wait (on the snapshot-shape probe, say), so
    // the deadline is checked here, where the process would start.
    if deadline.passed() {
        return Err("not started: the deadline had passed".to_string());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start: {error}"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        stop(&mut child)?;
        return Err("output pipes were not captured".to_string());
    };

    let buffers: [Arc<Mutex<Vec<u8>>>; 2] = Default::default();
    let total = Arc::new(AtomicUsize::new(0));
    let (sender, events) = mpsc::channel();
    let mut readers = [
        Some(spawn_reader(
            stdout,
            0,
            &buffers[0],
            &total,
            output_limit,
            &sender,
        )),
        Some(spawn_reader(
            stderr,
            1,
            &buffers[1],
            &total,
            output_limit,
            &sender,
        )),
    ];
    // Only the readers hold senders now, so a reader that dies without
    // reporting its end disconnects the channel once the other has finished.
    drop(sender);

    let mut finished = [false; 2];
    let end = wait(&mut child, &events, &mut finished, deadline);
    for (reader, finished) in readers.iter_mut().zip(finished) {
        if finished
            && let Some(reader) = reader.take()
            && reader.join().is_err()
        {
            return Err("an output reader panicked".to_string());
        }
    }
    let output = |index: usize| {
        buffers[index]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    };
    Ok(Captured {
        end: end?,
        stdout: output(0),
        stderr: output(1),
    })
}

fn spawn_reader(
    mut source: impl Read + Send + 'static,
    stream: usize,
    buffer: &Arc<Mutex<Vec<u8>>>,
    total: &Arc<AtomicUsize>,
    output_limit: usize,
    events: &mpsc::Sender<ReaderEvent>,
) -> JoinHandle<()> {
    let buffer = Arc::clone(buffer);
    let total = Arc::clone(total);
    let events = events.clone();
    std::thread::spawn(move || {
        let mut chunk = [0; 8192];
        let result = loop {
            match source.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(read) => {
                    let before = total.fetch_add(read, Ordering::Relaxed);
                    let kept = read.min(output_limit.saturating_sub(before));
                    buffer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&chunk[..kept]);
                    // The counter is shared, so exactly one read crosses the
                    // limit. Reading continues, so the writer never blocks on
                    // a full pipe before it is killed.
                    if before <= output_limit && kept < read {
                        // A send fails only once the capture has stopped
                        // listening, when nothing waits for the event.
                        drop(events.send(ReaderEvent::Exceeded));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error),
            }
        };
        drop(events.send(ReaderEvent::Finished(stream, result)));
    })
}

/// Waits for `child` to end, recording in `finished` which readers reported
/// the end of their stream. Every failure state is final: a child killed here
/// is reported as cut short, never as exited.
fn wait(
    child: &mut Child,
    events: &mpsc::Receiver<ReaderEvent>,
    finished: &mut [bool; 2],
    deadline: Deadline,
) -> Result<End, String> {
    while !finished.iter().all(|done| *done) {
        let Some(left) = deadline.remaining() else {
            return match child.try_wait() {
                Ok(Some(status)) => Ok(End::Cut(Cut::CaptureUnfinished(status))),
                Ok(None) => stop(child).map(|()| End::Cut(Cut::Unfinished)),
                Err(error) => stop_after(child, format!("could not check the process: {error}")),
            };
        };
        match events.recv_timeout(left) {
            Ok(ReaderEvent::Exceeded) => {
                return stop(child).map(|()| End::Cut(Cut::OutputExceeded));
            }
            Ok(ReaderEvent::Finished(stream, Ok(()))) => finished[stream] = true,
            Ok(ReaderEvent::Finished(stream, Err(error))) => {
                finished[stream] = true;
                return stop_after(
                    child,
                    format!("could not read {}: {error}", STREAM_NAMES[stream]),
                );
            }
            // The loop re-reads the deadline.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return stop_after(child, "an output reader panicked".to_string());
            }
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(End::Exited(status)),
            Ok(None) if deadline.passed() => {
                return stop(child).map(|()| End::Cut(Cut::Unfinished));
            }
            Ok(None) => std::thread::sleep(EXIT_POLL),
            Err(error) => {
                return stop_after(child, format!("could not check the process: {error}"));
            }
        }
    }
}

/// Kills `child` if it is still running and reaps it.
fn stop(child: &mut Child) -> Result<(), String> {
    if let Err(error) = child.kill()
        && error.kind() != io::ErrorKind::InvalidInput
    {
        return Err(format!("could not stop the process: {error}"));
    }
    child
        .wait()
        .map(|_| ())
        .map_err(|error| format!("could not reap the stopped process: {error}"))
}

/// Stops `child`, then reports `failure` (with any failure to stop it).
fn stop_after(child: &mut Child, failure: String) -> Result<End, String> {
    match stop(child) {
        Ok(()) => Err(failure),
        Err(stop_failure) => Err(format!("{failure}; {stop_failure}")),
    }
}

pub(crate) fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command()
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(args)
        .output()
        .expect("dellingr CLI should run")
}

/// Spawns the CLI with `source` already written to its stdin and the pipe
/// closed, so the child sees end of input. Tests hand the CLI Lua source this
/// way (as the script `-`) rather than through a file. Stdout and stderr are
/// piped; the caller may drop either end or collect the output.
pub(crate) fn spawn_stdin<I, S>(args: I, source: &[u8]) -> Child
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = command()
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("dellingr CLI should run");
    // Dropping the handle after the write closes the pipe. The CLI reads stdin
    // to its end before running anything, so a short script cannot block. A
    // run that is rejected at argument parsing never reads stdin and may have
    // exited already; that broken pipe is the expected outcome, not a failure.
    let written = child
        .stdin
        .take()
        .expect("stdin was configured as a pipe")
        .write_all(source);
    if let Err(error) = written {
        assert_eq!(
            error.kind(),
            io::ErrorKind::BrokenPipe,
            "stdin input should be written"
        );
    }
    child
}

/// Runs the CLI with `source` on stdin and collects its output.
pub(crate) fn run_stdin<I, S>(args: I, source: impl AsRef<[u8]>) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    spawn_stdin(args, source.as_ref())
        .wait_with_output()
        .expect("dellingr CLI should finish")
}
