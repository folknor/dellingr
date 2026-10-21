//! Integration tests that run all Lua example files.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

// Only `limited_command` is used here; the plain run helpers serve the CLI suites.
#[allow(dead_code)]
#[path = "common/cli.rs"]
mod cli;
mod common;

const DEFAULT_EXAMPLE_WORKERS: usize = 8;
const DEFAULT_EXAMPLE_TIMEOUT_MS: u64 = 60_000;
const POLL_INTERVAL_MS: u64 = 25;

#[test]
fn run_feature_test_extended_in_process() {
    let source = fs::read_to_string("examples/feature_test_extended.lua")
        .expect("Failed to read feature_test_extended.lua");
    let mut state = dellingr::State::new();
    state
        .load_string_named(
            &source,
            Some("examples/feature_test_extended.lua".to_string()),
        )
        .expect("feature_test_extended.lua should load successfully");
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect("feature_test_extended.lua should execute successfully");
}

fn collect_lua_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("Failed to read directory");
    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_lua_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "lua") {
            out.push(path);
        }
    }
}

fn read_output<R>(mut reader: R) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .expect("example process output should be readable");
        bytes
    })
}

struct ExampleRun {
    output: Output,
    timed_out: bool,
    timeout_ms: u64,
}

fn stop_example(child: &mut Child) -> Result<ExitStatus, String> {
    if let Err(error) = child.kill()
        && error.kind() != std::io::ErrorKind::InvalidInput
    {
        return Err(format!("could not stop example process: {error}"));
    }
    child
        .wait()
        .map_err(|error| format!("could not wait for stopped example: {error}"))
}

fn run_example(path: &Path, timeout_ms: u64) -> Result<ExampleRun, String> {
    let mut child = cli::limited_command()
        .arg("--quiet")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start dellingr example: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "example stdout was not captured".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "example stderr was not captured".to_string())?;
    let stdout_reader = read_output(stdout);
    let stderr_reader = read_output(stderr);
    let poll_count = timeout_ms.saturating_add(POLL_INTERVAL_MS - 1) / POLL_INTERVAL_MS;
    let mut status = None;

    for _ in 0..poll_count {
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS)),
            Err(error) => {
                if let Err(stop_error) = stop_example(&mut child) {
                    return Err(format!(
                        "could not check example process: {error}; {stop_error}"
                    ));
                }
                let stdout_result = stdout_reader.join();
                let stderr_result = stderr_reader.join();
                if stdout_result.is_err() || stderr_result.is_err() {
                    return Err(format!(
                        "could not check example process: {error}; output reader panicked"
                    ));
                }
                return Err(format!("could not check example process: {error}"));
            }
        }
    }

    let timed_out = status.is_none();
    if timed_out {
        status = Some(stop_example(&mut child)?);
    }

    let stdout = stdout_reader
        .join()
        .map_err(|_| "example stdout reader panicked".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "example stderr reader panicked".to_string())?;
    let output = Output {
        status: status.ok_or_else(|| "example process status unavailable after waiting".to_string())?,
        stdout,
        stderr,
    };

    Ok(ExampleRun {
        output,
        timed_out,
        timeout_ms,
    })
}

fn check_example(path: &Path, timeout_ms: u64) -> Option<String> {
    // Every collected path starts with examples/; a stray one prints whole.
    // A hand-written match trips clippy's manual_unwrap_or, and the
    // fallback here is intentional, so allow the banned method locally.
    #[allow(clippy::disallowed_methods)]
    let display = path.strip_prefix("examples").unwrap_or(path).display();

    println!("Running: {display}");

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return Some(format!("{display}: could not read source: {error}")),
    };
    let defines_bench = source.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("function _bench(") || line.starts_with("local function _bench(")
    });
    let run = match run_example(path, timeout_ms) {
        Ok(run) => run,
        Err(error) => return Some(format!("{display}: {error}")),
    };
    let output = run.output;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if run.timed_out {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(format!(
            "{display}: timed out after {} ms\nstdout: {stdout}\nstderr: {stderr}",
            run.timeout_ms
        ))
    } else if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(format!(
            "{}: exit code {:?}\nstdout: {}\nstderr: {}",
            display,
            output.status.code(),
            stdout,
            stderr
        ))
    } else if !stderr.is_empty() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(format!(
            "{display}: unexpected stderr\nstdout: {stdout}\nstderr: {stderr}"
        ))
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Example assertions follow the convention `name: false`.
        if stdout.lines().any(|line| line.ends_with(": false")) {
            Some(format!(
                "{display}: test assertion failed\noutput: {stdout}"
            ))
        } else if defines_bench && !stdout.lines().any(|line| line.ends_with(": true")) {
            Some(format!(
                "{display}: _bench example did not print a success line\noutput: {stdout}"
            ))
        } else {
            None
        }
    }
}

#[allow(clippy::disallowed_methods)]
fn example_worker_count(file_count: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map_or(DEFAULT_EXAMPLE_WORKERS, std::num::NonZeroUsize::get);
    let default = available.min(DEFAULT_EXAMPLE_WORKERS);
    let configured = std::env::var("DELLINGR_EXAMPLE_WORKERS");
    let workers = match configured {
        Ok(value) => {
            let workers = value.parse::<usize>().unwrap_or_else(|_| {
                panic!("DELLINGR_EXAMPLE_WORKERS must be between 1 and 8, got {value:?}")
            });
            assert!(
                workers > 0,
                "DELLINGR_EXAMPLE_WORKERS must be between 1 and 8"
            );
            assert!(
                workers <= DEFAULT_EXAMPLE_WORKERS,
                "DELLINGR_EXAMPLE_WORKERS must be between 1 and 8"
            );
            workers.min(default)
        }
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("DELLINGR_EXAMPLE_WORKERS must be valid Unicode")
        }
    };
    workers.min(file_count)
}

#[allow(clippy::disallowed_methods)]
fn example_timeout_ms() -> u64 {
    match std::env::var("DELLINGR_EXAMPLE_TIMEOUT_MS") {
        Ok(value) => {
            let timeout_ms = value.parse::<u64>().unwrap_or_else(|_| {
                panic!("DELLINGR_EXAMPLE_TIMEOUT_MS must be a positive integer, got {value:?}")
            });
            assert!(
                timeout_ms > 0,
                "DELLINGR_EXAMPLE_TIMEOUT_MS must be a positive integer"
            );
            timeout_ms
        }
        Err(std::env::VarError::NotPresent) => DEFAULT_EXAMPLE_TIMEOUT_MS,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("DELLINGR_EXAMPLE_TIMEOUT_MS must be valid Unicode")
        }
    }
}

fn run_examples_bucket(name: &str, include: impl Fn(&Path) -> bool) {
    let examples_dir = Path::new("examples");

    assert!(examples_dir.exists(), "examples directory not found");

    let mut files: Vec<PathBuf> = Vec::new();
    collect_lua_files(examples_dir, &mut files);
    files.retain(|path| include(path));
    files.sort();

    assert!(
        !files.is_empty(),
        "No .lua files found in examples bucket {name}"
    );

    // Keep stress workload concurrency bounded and let constrained CI lower
    // it without changing the source tree.
    let workers = example_worker_count(files.len());
    let timeout_ms = example_timeout_ms();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut failures: Vec<(usize, String)> = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = files.get(i) else { break };
                        if let Some(failure) = check_example(path, timeout_ms) {
                            local.push((i, failure));
                        }
                    }
                    local
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("example worker panicked"))
            .collect()
    });
    failures.sort_by_key(|(i, _)| *i);
    let failures: Vec<String> = failures.into_iter().map(|(_, failure)| failure).collect();

    if !failures.is_empty() {
        panic!(
            "\n{} example(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }

    println!("\nAll {} examples passed in {name}!", files.len());
}

#[test]
fn run_all_examples() {
    run_examples_bucket("all", |_| true);
}
