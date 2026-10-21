//! Bench harness for dellingr: the binary brokkr invokes for `--bench` /
//! `--hotpath` / `--alloc` runs, and the KV source for scripts/hotbench.sh.
//!
//! Loads a bench script that must define a global `_bench` function, then
//! runs phases on one `State`:
//!   - parse   `load_string_named` (parser and codegen)
//!   - setup   top-level chunk run (registers `_bench`, builds fixtures)
//!   - cold    first `_bench` invocation
//!   - warm    N further invocations on the same State, controlled by
//!     `--iterations N`
//!
//! A script path is required and is resolved from the caller's working
//! directory. brokkr and hotbench.sh both pass the registered file path.
//!
//! Output:
//!   - KV pairs on stderr for phase timings and heap/object brackets.
//!   - Optionally, the brokkr sidecar stream: when `BROKKR_MARKER_FIFO`
//!     names a writable path, phase markers (`PARSE_START`/`PARSE_END`,
//!     `SETUP_*`, `COLD_*`, `WARM_*`, plus batched `WARM_BLOCK_*` sub-spans)
//!     and `@name=value` counters are written to it. Without the env var,
//!     every emit is a no-op. Open or write failures are reported and fail
//!     the run so brokkr cannot record an incomplete marker stream.
//!
//! Wall time is the benchmarking metric. The cost KVs ride along as a
//! workload fingerprint - cost is a deterministic property of the script,
//! so a cost change between runs of the "same" bench means the workload or
//! the compiler changed, not the VM's speed.
//!
//! Run through `scripts/hotbench.sh <workload-name>` or a brokkr workload
//! command; both resolve a `brokkr.toml` workload registration to the script
//! path this harness requires.

// A benchmark harness is a host process: it legitimately reads its own
// command line, the marker FIFO from the environment, and wall-clock time.
// The clippy.toml bans on those guard the deterministic library, not this.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
// Exit status is 1 for a failed run and 2 for a usage error. The process
// status lets the harness caller distinguish a complete run from a failure.
use std::process::exit;
use std::time::{Duration, Instant};

use dellingr::{ArgCount, LuaType, RetCount, State};

// Install the counting allocator in this instrumented binary so
// hotpath-alloc runs observe allocation traffic. Without it, allocations use
// the system allocator and the per-function allocation counters stay empty.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const DEFAULT_ITERATIONS: u32 = 20;
const BENCH_FUNCTION: &str = "_bench";
/// Upper bound on `WARM_BLOCK` marker pairs per run; keeps the sidecar
/// stream at tens of spans regardless of the iteration count.
const MAX_WARM_BLOCKS: u32 = 32;
/// Maximum time to wait for room in the marker FIFO for one line.
const FIFO_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Emit a named phase marker to the brokkr sidecar (if active).
///
/// Protocol: `<timestamp_us> <name>\n` on the FIFO named by
/// `BROKKR_MARKER_FIFO`, timestamped in microseconds since the first emit.
/// Counters use `<timestamp_us> @name=value\n`. The parser lives in brokkr (a
/// separate repository); the line format is tiny and stable, and a shared
/// protocol crate for two one-line formats would cost more than it protects.
fn emit_marker(name: &str) {
    write_fifo(|us| format!("{us} {name}\n"));
}

/// Emit a named counter value to the brokkr sidecar (if active).
///
/// Protocol: `<timestamp_us> @<name>=<value>\n`.
fn emit_counter(name: &str, value: i64) {
    write_fifo(|us| format!("{us} @{name}={value}\n"));
}

/// Shared FIFO write logic for markers and counters. No-op when
/// `BROKKR_MARKER_FIFO` is unset. A configured but unusable FIFO fails the run;
/// a full FIFO is retried for at most `FIFO_WRITE_TIMEOUT`.
fn write_fifo(line: impl FnOnce(u128) -> String) {
    use std::sync::OnceLock;

    static STATE: OnceLock<Option<(std::fs::File, Instant)>> = OnceLock::new();

    let state = STATE.get_or_init(|| {
        let path = env::var_os("BROKKR_MARKER_FIFO")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(file) => Some((file, Instant::now())),
                Err(error) => {
                    eprintln!(
                        "Failed to open BROKKR_MARKER_FIFO {}: {error}",
                        PathBuf::from(&path).display()
                    );
                    exit(1);
                }
            }
        }
        #[cfg(not(unix))]
        {
            eprintln!(
                "BROKKR_MARKER_FIFO is unsupported on this platform: {}",
                PathBuf::from(&path).display()
            );
            exit(1);
        }
    });

    if let Some((file, start)) = state.as_ref() {
        let us = start.elapsed().as_micros();
        let line = line(us);
        if let Err(error) = write_fifo_all(file, line.as_bytes()) {
            eprintln!("Failed to write BROKKR_MARKER_FIFO: {error}");
            exit(1);
        }
    }
}

fn write_fifo_all(mut file: &fs::File, bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + FIFO_WRITE_TIMEOUT;
    let mut written = 0;
    while written < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for marker FIFO space",
            ));
        }
        match file.write(&bytes[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1).min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Convert a metric to the sidecar's signed counter type. A value past
/// `i64::MAX` fails the run: storing a clamped counter would record a
/// plausible-looking but wrong number.
fn counter_value(name: &str, v: u64) -> i64 {
    match i64::try_from(v) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Benchmark counter {name}={v} does not fit the sidecar's i64 counter");
            exit(1);
        }
    }
}

struct Args {
    script_path: PathBuf,
    /// Display name for the `target=` KV.
    target: String,
    iterations: u32,
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut positional: Option<String> = None;
    let mut iterations = DEFAULT_ITERATIONS;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--iterations" {
            match args.next().map(|v| v.parse::<u32>()) {
                Some(Ok(n)) if n > 0 => iterations = n,
                _ => return Err("--iterations requires a positive integer".to_owned()),
            }
        } else if positional.is_none() {
            positional = Some(arg);
        } else {
            return Err(format!("unexpected argument: {arg}"));
        }
    }

    let raw =
        positional.ok_or_else(|| "usage: hotpath <script-path> [--iterations N]".to_owned())?;
    Ok(Args {
        script_path: PathBuf::from(&raw),
        target: raw,
        iterations,
    })
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(env::args().skip(1))
}

struct Metric {
    name: &'static str,
    value: String,
    sidecar_counter: Option<i64>,
}

fn text_metric(name: &'static str, value: String) -> Metric {
    Metric {
        name,
        value,
        sidecar_counter: None,
    }
}

fn integer_metric(name: &'static str, value: u128, sidecar_counter: Option<i64>) -> Metric {
    Metric {
        name,
        value: value.to_string(),
        sidecar_counter,
    }
}

struct RunMetrics<'a> {
    target: &'a str,
    state_new_us: u128,
    parse_us: u128,
    cold_call_us: u128,
    cold_cost: u64,
    warm_iterations: u32,
    warm_avg_us: f64,
    warm_avg_cost: u64,
    cost_per_us: f64,
    setup_heap_bytes: usize,
    setup_object_count: usize,
    final_heap_bytes: usize,
    final_object_count: usize,
}

fn run_metrics(run: &RunMetrics<'_>) -> Vec<Metric> {
    vec![
        text_metric("target", run.target.to_owned()),
        integer_metric("state_new_us", run.state_new_us, None),
        integer_metric("parse_us", run.parse_us, None),
        integer_metric("cold_call_us", run.cold_call_us, None),
        integer_metric("cold_cost", u128::from(run.cold_cost), None),
        integer_metric(
            "warm_iterations",
            u128::from(run.warm_iterations),
            Some(i64::from(run.warm_iterations)),
        ),
        // brokkr parses decimal key-value metrics as f64.
        text_metric("warm_avg_us", format!("{:.3}", run.warm_avg_us)),
        integer_metric(
            "warm_avg_cost",
            u128::from(run.warm_avg_cost),
            Some(counter_value("warm_avg_cost", run.warm_avg_cost)),
        ),
        text_metric("cost_per_us", format!("{:.2}", run.cost_per_us)),
        integer_metric(
            "setup_heap_bytes",
            run.setup_heap_bytes as u128,
            Some(counter_value(
                "setup_heap_bytes",
                run.setup_heap_bytes as u64,
            )),
        ),
        integer_metric(
            "setup_object_count",
            run.setup_object_count as u128,
            Some(counter_value(
                "setup_object_count",
                run.setup_object_count as u64,
            )),
        ),
        integer_metric(
            "final_heap_bytes",
            run.final_heap_bytes as u128,
            Some(counter_value(
                "final_heap_bytes",
                run.final_heap_bytes as u64,
            )),
        ),
        integer_metric(
            "final_object_count",
            run.final_object_count as u128,
            Some(counter_value(
                "final_object_count",
                run.final_object_count as u64,
            )),
        ),
    ]
}

fn write_kvs(writer: &mut impl Write, metrics: &[Metric]) -> io::Result<()> {
    for metric in metrics {
        writeln!(writer, "{}={}", metric.name, metric.value)?;
    }
    Ok(())
}

// Writing the KV report to stderr is this harness's job.
#[allow(clippy::disallowed_methods)]
fn emit_metrics(metrics: &[Metric]) {
    let stderr = io::stderr();
    let mut output = stderr.lock();
    let result = write_kvs(&mut output, metrics);
    drop(output);

    if let Err(error) = result {
        eprintln!("Failed to write benchmark metrics to stderr: {error}");
        exit(1);
    }

    for metric in metrics {
        if let Some(value) = metric.sidecar_counter {
            emit_counter(metric.name, value);
        }
    }
}

fn find_bench(state: &mut State, script_path: &std::path::Path, phase: &str) -> Result<(), String> {
    state.get_global(BENCH_FUNCTION).map_err(|error| {
        format!(
            "Failed to look up `{BENCH_FUNCTION}` before the {phase} call in {}: {error}",
            script_path.display()
        )
    })?;
    let actual_type = state.typ(-1);
    if actual_type != LuaType::Function {
        return Err(format!(
            "Script {} must define a global `{BENCH_FUNCTION}` function; found {actual_type}",
            script_path.display()
        ));
    }
    Ok(())
}

fn require_bench(state: &mut State, script_path: &std::path::Path, phase: &str) {
    if let Err(error) = find_bench(state, script_path, phase) {
        eprintln!("{error}");
        exit(1);
    }
}

/// Runs `n` warm calls of the cached bench function on top of the stack and
/// returns the time spent in the calls alone.
fn run_warm_block(
    state: &mut State,
    script_path: &std::path::Path,
    n: u32,
) -> Result<Duration, String> {
    let block_start = Instant::now();
    for _ in 0..n {
        // Keep the resolved function rooted and copy it for each call.
        state.push_value(-1).map_err(|error| {
            format!(
                "Failed to copy cached `{BENCH_FUNCTION}` for a warm call in {}: {error}",
                script_path.display()
            )
        })?;
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .map_err(|error| {
                format!(
                    "Warm `{BENCH_FUNCTION}` call error in {}: {error}",
                    script_path.display()
                )
            })?;
    }
    Ok(block_start.elapsed())
}

fn main() {
    let _guard = hotpath::HotpathGuardBuilder::new("dellingr::hotpath")
        .percentiles(&[50.0, 95.0, 99.0])
        .functions_limit(0)
        .build();

    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            exit(2);
        }
    };
    let target = &args.target;

    let source = match fs::read_to_string(&args.script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", args.script_path.display());
            exit(1);
        }
    };

    let state_start = Instant::now();
    let mut state = State::new();
    let state_new_us = state_start.elapsed().as_micros();

    // Phase 1: parse + codegen.
    let chunk_name = format!("@{}", args.script_path.display());
    emit_marker("PARSE_START");
    let parse_start = Instant::now();
    if let Err(e) = state.load_string_named(&source, Some(chunk_name)) {
        eprintln!("Parse error in {}: {e}", args.script_path.display());
        exit(1);
    }
    let parse_us = parse_start.elapsed().as_micros();
    emit_marker("PARSE_END");

    // Phase 2: run the chunk to register `_bench` (and any per-script setup).
    emit_marker("SETUP_START");
    if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        eprintln!("Chunk run error in {}: {e}", args.script_path.display());
        exit(1);
    }
    emit_marker("SETUP_END");
    let setup_cost = state.cost_used();
    let setup_heap_bytes = state.heap_size();
    let setup_object_count = state.object_count();

    // Phase 3: cold call - first invocation of `_bench`.
    require_bench(&mut state, &args.script_path, "cold");

    emit_marker("COLD_START");
    let cold_start = Instant::now();
    if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        eprintln!(
            "Cold `{BENCH_FUNCTION}` call error in {}: {e}",
            args.script_path.display()
        );
        exit(1);
    }
    let cold_call_us = cold_start.elapsed().as_micros();
    emit_marker("COLD_END");
    let cold_cost = state.cost_used() - setup_cost;

    // Phase 4: warm calls, batched into at most MAX_WARM_BLOCKS sub-spans so
    // the sidecar sees within-process spread without flooding the FIFO.
    let iterations = args.iterations;
    require_bench(&mut state, &args.script_path, "warm");
    let block_size = (iterations / MAX_WARM_BLOCKS).max(1);
    emit_marker("WARM_START");
    let mut warm_elapsed = Duration::ZERO;
    let mut done = 0u32;
    while done < iterations {
        let n = block_size.min(iterations - done);
        emit_marker("WARM_BLOCK_START");
        // Measure only the calls. FIFO writes can wait on the reader, so their
        // time stays outside the summed warm duration.
        match run_warm_block(&mut state, &args.script_path, n) {
            Ok(elapsed) => warm_elapsed += elapsed,
            Err(error) => {
                eprintln!("{error}");
                exit(1);
            }
        }
        emit_marker("WARM_BLOCK_END");
        done += n;
    }
    let warm_total_us = warm_elapsed.as_secs_f64() * 1_000_000.0;
    if let Err(error) = state.pop(1) {
        eprintln!(
            "Failed to release cached `{BENCH_FUNCTION}` after warm calls in {}: {error}",
            args.script_path.display()
        );
        exit(1);
    }
    emit_marker("WARM_END");
    let warm_avg_us = warm_total_us / f64::from(iterations);
    let warm_total_cost = state.cost_used() - setup_cost - cold_cost;
    let warm_avg_cost = warm_total_cost / u64::from(iterations);

    let cost_per_us = if warm_avg_us > 0.0 {
        warm_avg_cost as f64 / warm_avg_us
    } else {
        0.0
    };

    let final_heap_bytes = state.heap_size();
    let final_object_count = state.object_count();
    let metrics = run_metrics(&RunMetrics {
        target,
        state_new_us,
        parse_us,
        cold_call_us,
        cold_cost,
        warm_iterations: iterations,
        warm_avg_us,
        warm_avg_cost,
        cost_per_us,
        setup_heap_bytes,
        setup_object_count,
        final_heap_bytes,
        final_object_count,
    });
    emit_metrics(&metrics);
}

#[cfg(test)]
mod tests {
    use super::{RunMetrics, find_bench, parse_args_from, run_metrics, write_kvs};
    use dellingr::State;
    use std::path::PathBuf;

    #[test]
    fn hotpath_requires_a_script_path() {
        assert!(parse_args_from(Vec::<String>::new()).is_err());
    }

    #[test]
    fn hotpath_keeps_the_callers_script_path() {
        let args = parse_args_from(["examples/numerics/arithmetic.lua".to_owned()])
            .expect("script path parses");
        assert_eq!(
            args.script_path,
            PathBuf::from("examples/numerics/arithmetic.lua")
        );
    }

    #[test]
    fn missing_bench_function_is_an_error() {
        let mut state = State::new();
        assert!(find_bench(&mut state, std::path::Path::new("no_bench.lua"), "cold").is_err());
    }

    #[test]
    fn stderr_metrics_have_the_expected_key_set() {
        let run = RunMetrics {
            target: "examples/numerics/arithmetic.lua",
            state_new_us: 1,
            parse_us: 2,
            cold_call_us: 3,
            cold_cost: 4,
            warm_iterations: 20,
            warm_avg_us: 5.5,
            warm_avg_cost: 6,
            cost_per_us: 1.09,
            setup_heap_bytes: 7,
            setup_object_count: 8,
            final_heap_bytes: 9,
            final_object_count: 10,
        };
        let metrics = run_metrics(&run);
        let mut stderr = Vec::new();
        write_kvs(&mut stderr, &metrics).expect("metric output is writable");
        let output = String::from_utf8(stderr).expect("metric output is UTF-8");
        let keys = output
            .lines()
            .map(|line| {
                line.split_once('=')
                    .expect("each metric line has a key separator")
                    .0
            })
            .collect::<Vec<_>>();
        assert_eq!(
            keys.as_slice(),
            &[
                "target",
                "state_new_us",
                "parse_us",
                "cold_call_us",
                "cold_cost",
                "warm_iterations",
                "warm_avg_us",
                "warm_avg_cost",
                "cost_per_us",
                "setup_heap_bytes",
                "setup_object_count",
                "final_heap_bytes",
                "final_object_count",
            ]
        );
    }
}
