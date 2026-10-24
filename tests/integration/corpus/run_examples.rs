//! Integration tests that run all Lua example files.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use dellingr::Program;

use crate::support::{self, examples, process};

const EXAMPLE_WORKERS: usize = 8;
const EXAMPLE_TIMEOUT_MS: u64 = 60_000;
const EXAMPLE_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL_MS: u64 = 25;

#[test]
fn run_feature_test_extended_in_process() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let feature_test = examples::example_corpus()
        .into_iter()
        .find(|path| {
            path.strip_prefix(&examples_dir)
                .is_ok_and(|relative| relative == Path::new("feature_test_extended.lua"))
        })
        .expect("feature_test_extended.lua must be present in the example corpus");
    let source =
        fs::read_to_string(&feature_test).expect("Failed to read feature_test_extended.lua");
    let mut state = dellingr::State::new();
    let source_name = feature_test
        .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("example corpus paths must be rooted at the crate manifest")
        .display()
        .to_string();
    dellingr::Program::compile(&source, Some(source_name.as_bytes()))
        .and_then(|program| state.load(&program))
        .expect("feature_test_extended.lua should load successfully");
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect("feature_test_extended.lua should execute successfully");
}

fn read_output<R>(
    mut reader: R,
    captured_bytes: Arc<AtomicUsize>,
    output_exceeded: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let read = reader
                .read(&mut buffer)
                .expect("example process output should be readable");
            if read == 0 {
                break;
            }

            let previous = captured_bytes.fetch_add(read, Ordering::Relaxed);
            let remaining = EXAMPLE_OUTPUT_LIMIT_BYTES.saturating_sub(previous);
            let captured = read.min(remaining);
            bytes.extend_from_slice(&buffer[..captured]);
            if captured < read {
                output_exceeded.store(true, Ordering::Release);
            }
        }
        bytes
    })
}

struct ExampleRun {
    output: Output,
    output_exceeded: bool,
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
    let mut child = process::limited_command()
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
    let captured_bytes = Arc::new(AtomicUsize::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = read_output(
        stdout,
        Arc::clone(&captured_bytes),
        Arc::clone(&output_exceeded),
    );
    let stderr_reader = read_output(stderr, captured_bytes, Arc::clone(&output_exceeded));
    let poll_count = timeout_ms.saturating_add(POLL_INTERVAL_MS - 1) / POLL_INTERVAL_MS;
    let mut status = None;

    for _ in 0..poll_count {
        if output_exceeded.load(Ordering::Acquire) {
            status = Some(stop_example(&mut child)?);
            break;
        }
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
        status: status
            .ok_or_else(|| "example process status unavailable after waiting".to_string())?,
        stdout,
        stderr,
    };

    Ok(ExampleRun {
        output,
        output_exceeded: output_exceeded.load(Ordering::Acquire),
        timed_out,
        timeout_ms,
    })
}

fn defines_global_bench(source: &str) -> bool {
    if !source.contains("_bench") {
        return false;
    }

    let Ok(program) = Program::compile(source, None) else {
        return false;
    };
    let listing = program.disassemble().to_string();
    let root = listing
        .split_once("\nfunction main/")
        .map_or(listing.as_str(), |(root, _)| root);

    // Root bytecode writes distinguish a global bench from a local function.
    root.lines().any(|line| {
        let Some((instruction, annotation)) = line.split_once("  ; ") else {
            return false;
        };
        let writes_global = instruction
            .split_whitespace()
            .any(|part| part == "SET_GLOBAL");
        let global_name = annotation
            .split_once(", ")
            .map_or(annotation, |(name, _)| name);
        writes_global && global_name == "\"_bench\""
    })
}

fn check_example(path: &Path, timeout_ms: u64) -> Option<String> {
    let display = path
        .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("example corpus paths must be rooted at the crate manifest")
        .display();

    println!("Running: {display}");

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return Some(format!("{display}: could not read source: {error}")),
    };
    let defines_bench = defines_global_bench(&source);
    let run = match run_example(path, timeout_ms) {
        Ok(run) => run,
        Err(error) => return Some(format!("{display}: {error}")),
    };
    let output = run.output;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if run.output_exceeded {
        Some(format!(
            "{display}: stdout and stderr exceeded the combined output limit of {EXAMPLE_OUTPUT_LIMIT_BYTES} bytes"
        ))
    } else if run.timed_out {
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

fn run_examples_bucket(name: &str) {
    let files = examples::example_corpus();

    assert!(
        !files.is_empty(),
        "No .lua files found in examples bucket {name}"
    );

    // Keep stress workload concurrency bounded by available parallelism.
    let failures = support::parallel_failures(&files, EXAMPLE_WORKERS, |path| {
        check_example(path, EXAMPLE_TIMEOUT_MS)
    });

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
    run_examples_bucket("all");
}
