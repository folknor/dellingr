//! Bench harness for dellingr: the binary `brokkr bench` runs for the
//! `wall`, `kernel` and `alloc` benches (brokkr.d/bench.toml). Built only
//! under the `bench-harness` feature, so `cargo install` skips it.
//!
//! Loads a bench script that must define a global `_bench` function, then
//! runs phases on one `State`:
//!   - parse   `Program::compile` plus `State::load` (parser and codegen)
//!   - setup   top-level chunk run (registers `_bench`, builds fixtures)
//!   - cold    first `_bench` invocation
//!   - warm    N further invocations on the same State, controlled by
//!     `--iterations N`
//!
//! A script carrying the line `-- Harness host: records` gets the host
//! function `_host_decode` installed as a global before the parse phase, so
//! a workload can drive host callbacks reading its tables; every other
//! script runs on a plain `State::new()`.
//!
//! A file carrying `-- Harness scenario: <kind>` is a Rust-driven scenario
//! instead (the `host` and `snapshot` benches): no `_bench`; its top level
//! runs once as the fixture and sets a `count` global, then the kind's
//! driver performs `count` host operations (`ScenarioKind` lists them),
//! calls the file's optional `verify`, and prints KVs and a
//! `host/<stem>: <count>: true` line.
//!
//! A script path is required and is resolved from the caller's working
//! directory. brokkr passes the bound workload's pinned file path.
//!
//! Output:
//!   - KV pairs on stderr for phase timings and heap/object brackets, which
//!     brokkr.d/bench.lua stores as the iteration's record.
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
//! Run through `brokkr bench <bench> --workload <name>`, which resolves the
//! `[data.workloads.*]` registration to the script path this harness
//! requires.

#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "A benchmark harness is a host process: it legitimately reads its own command line, the marker FIFO from the environment, and wall-clock time, and writes its KV report to stderr. The clippy.toml bans on those guard the deterministic library, not this."
)]

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
// Exit status is 1 for a failed run and 2 for a usage error. The process
// status lets the harness caller distinguish a complete run from a failure.
use std::process::exit;
use std::time::{Duration, Instant};

use dellingr::{ArgCount, FieldName, LuaType, Program, RetCount, State};

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

/// The marker clock's origin, taken first thing in `main`. brokkr's sidecar
/// contract stamps every FIFO line in microseconds since the start of the
/// process it spawned and compares them directly with its `/proc` samples, so
/// the origin must not wait for the first marker: work before it (State
/// setup, reading the script) would shift every marker early by that delay.
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn process_start() -> Instant {
    *PROCESS_START.get_or_init(Instant::now)
}

/// Emit a named phase marker to the brokkr sidecar (if active).
///
/// Protocol: `<timestamp_us> <name>\n` on the FIFO named by
/// `BROKKR_MARKER_FIFO`, timestamped in microseconds since process start
/// (`PROCESS_START`).
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

    static STATE: OnceLock<Option<std::fs::File>> = OnceLock::new();

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
                Ok(file) => Some(file),
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

    if let Some(file) = state.as_ref() {
        let us = process_start().elapsed().as_micros();
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

    let raw = positional
        .ok_or_else(|| "usage: dellingr-bench <script-path> [--iterations N]".to_owned())?;
    Ok(Args {
        script_path: PathBuf::from(&raw),
        target: raw,
        iterations,
    })
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(env::args().skip(1))
}

fn chunk_name(script_path: &Path) -> &[u8] {
    script_path.as_os_str().as_encoded_bytes()
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
    warm_total_cost: u64,
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
        integer_metric("warm_total_cost", u128::from(run.warm_total_cost), None),
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

/// The line a workload carries to have the harness install its host
/// functions before the chunk runs. Workloads without it see the plain
/// `State::new()` environment, so their globals and costs are unchanged.
const HOST_RECORDS_DIRECTIVE: &[u8] = b"-- Harness host: records";

fn wants_host_records(source: &[u8]) -> bool {
    source
        .split(|&b| b == b'\n')
        .any(|line| line.trim_ascii_end() == HOST_RECORDS_DIRECTIVE)
}

/// The fields `_host_decode` reads, one descriptor per field: the way a
/// host decodes the same fields of many records.
static X: FieldName = FieldName::new("x");
static Y: FieldName = FieldName::new("y");
static VX: FieldName = FieldName::new("vx");
static VY: FieldName = FieldName::new("vy");
static HP: FieldName = FieldName::new("hp");
static PLAYER: FieldName = FieldName::new("player");
static KIND: FieldName = FieldName::new("kind");
static ALIVE: FieldName = FieldName::new("alive");

/// `_host_decode(e)`: the host side of a record-decoding workload. Reads
/// eight own fields of the entity table in one `State::raw_fields` batch
/// through `FieldName` descriptors and returns the sum the workload's Lua fallback
/// computes.
fn host_decode(state: &mut State) -> dellingr::Result<u8> {
    let (x, y, vx, vy, hp, player, kind, alive): (f64, f64, f64, f64, f64, u32, &str, bool) =
        state.raw_fields(1, (&X, &Y, &VX, &VY, &HP, &PLAYER, &KIND, &ALIVE))?;
    let kind_len = kind.len() as f64;
    let sum = x + y + vx + vy + hp + f64::from(player) + kind_len + f64::from(u8::from(alive));
    state.push_number(sum)?;
    Ok(1)
}

fn install_host_records(state: &mut State) -> dellingr::Result<()> {
    let handle = state.add_rust_fn("_host_decode", host_decode)?;
    state.push_rust_fn(handle)?;
    state.set_global("_host_decode")
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

/// The line prefix that makes a file a Rust-driven scenario rather than a
/// `_bench` workload: `-- Harness scenario: <kind>`, exactly one whole line.
const SCENARIO_DIRECTIVE: &str = "-- Harness scenario: ";
/// Upper bound on a scenario's `count`, so a typo cannot run for days and
/// the count converts to `u32` and `f64` exactly.
const MAX_SCENARIO_COUNT: u32 = 100_000_000;

/// What a scenario's driver does with `count` operations after the file's
/// top level has run. A kind's name is part of the measurement's identity:
/// a stored run pairs with another only through the pinned file, so a
/// change to what a kind does is a new kind name, never an edit under the
/// old one (an older binary would run the old work under the same name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScenarioKind {
    /// `State::new()` then drop: construction and teardown of a standard
    /// State.
    StateNew,
    /// `Program::compile` of the `source` string the file built, dropping
    /// each Program: parse and codegen without `State::load`.
    Compile,
    /// The file's `chunk_source` compiled `chunks` times as distinct
    /// Programs and run once each (each roots a closure), then `count`
    /// explicit full collections with automatic collection off: the
    /// per-collection work proportional to retained runtimes.
    ManyChunks,
    /// `State::invoke(&step, (i, 0.5))` returning one number.
    InvokeTyped,
    /// The stack sequence for the same call: push two numbers,
    /// `call_anchor`, `to_number`, `pop`.
    InvokeStack,
    /// `State::save_state` of the graph the file built.
    SnapshotSave,
    /// `State::load_state` of this build's own save of the file's graph,
    /// dropping each restored State (teardown included).
    SnapshotLoad,
}

const SCENARIO_KINDS: &[(&str, ScenarioKind)] = &[
    ("state_new", ScenarioKind::StateNew),
    ("compile", ScenarioKind::Compile),
    ("many_chunks", ScenarioKind::ManyChunks),
    ("invoke_typed", ScenarioKind::InvokeTyped),
    ("invoke_stack", ScenarioKind::InvokeStack),
    ("snapshot_save", ScenarioKind::SnapshotSave),
    ("snapshot_load", ScenarioKind::SnapshotLoad),
];

impl ScenarioKind {
    fn name(self) -> &'static str {
        SCENARIO_KINDS
            .iter()
            .find(|(_, kind)| *kind == self)
            .map(|(name, _)| *name)
            .expect("every kind is listed in SCENARIO_KINDS")
    }

    fn needs_snapshot(self) -> bool {
        matches!(self, Self::SnapshotSave | Self::SnapshotLoad)
    }
}

/// The scenario a file declares: `None` for an ordinary `_bench` workload,
/// an error for a malformed or repeated directive or an unknown kind (exact
/// match only, so a renamed kind never falls through to another driver).
fn scenario_kind(source: &[u8]) -> Result<Option<ScenarioKind>, String> {
    let mut found = None;
    for line in source.split(|&b| b == b'\n') {
        let line = line.trim_ascii_end();
        let Some(name) = line.strip_prefix(SCENARIO_DIRECTIVE.as_bytes()) else {
            continue;
        };
        if found.is_some() {
            return Err("a scenario file declares one `-- Harness scenario:` line".to_owned());
        }
        let name =
            std::str::from_utf8(name).map_err(|_| "the scenario kind is not UTF-8".to_owned())?;
        let kind = SCENARIO_KINDS
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, kind)| *kind)
            .ok_or_else(|| format!("unknown scenario kind `{name}`"))?;
        found = Some(kind);
    }
    Ok(found)
}

/// Reads and clears the file's `count` global: a positive integer no larger
/// than `MAX_SCENARIO_COUNT`. Clearing it keeps the scale dial out of any
/// snapshot the scenario takes.
fn take_count(state: &mut State) -> Result<u32, String> {
    state.get_global("count").map_err(|e| e.to_string())?;
    let value = state
        .to_number(-1)
        .map_err(|_| "a scenario file sets a numeric global `count`".to_owned())?;
    state.pop(1).map_err(|e| e.to_string())?;
    if !(value >= 1.0 && value <= f64::from(MAX_SCENARIO_COUNT) && value.fract() == 0.0) {
        return Err(format!(
            "`count` must be an integer from 1 to {MAX_SCENARIO_COUNT}, got {value}"
        ));
    }
    state.push_nil().map_err(|e| e.to_string())?;
    state.set_global("count").map_err(|e| e.to_string())?;
    Ok(value as u32)
}

fn global_bytes(state: &mut State, name: &str) -> Result<Vec<u8>, String> {
    state.get_global(name).map_err(|e| e.to_string())?;
    let bytes = state
        .to_bytes(-1)
        .map(<[u8]>::to_vec)
        .map_err(|_| format!("this scenario needs a string global `{name}`"))?;
    state.pop(1).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn global_count(state: &mut State, name: &str) -> Result<u32, String> {
    state.get_global(name).map_err(|e| e.to_string())?;
    let value = state
        .to_number(-1)
        .map_err(|_| format!("this scenario needs a numeric global `{name}`"))?;
    state.pop(1).map_err(|e| e.to_string())?;
    if !(value >= 1.0 && value <= f64::from(MAX_SCENARIO_COUNT) && value.fract() == 0.0) {
        return Err(format!("`{name}` must be a positive integer, got {value}"));
    }
    Ok(value as u32)
}

fn anchored_function(state: &mut State, name: &str) -> Result<dellingr::Anchor, String> {
    state.get_global(name).map_err(|e| e.to_string())?;
    state
        .anchor_function()
        .map_err(|_| format!("this scenario needs a global function `{name}`"))
}

/// Calls the file's `verify` function when it defines one, after setting
/// the global `result` the driver computed (nil when it computed none). A
/// verify that errors fails the run.
fn run_verify(state: &mut State, result: Option<f64>) -> Result<(), String> {
    match result {
        Some(value) => state.push_number(value),
        None => state.push_nil(),
    }
    .map_err(|e| e.to_string())?;
    state.set_global("result").map_err(|e| e.to_string())?;
    state.get_global("verify").map_err(|e| e.to_string())?;
    if state.typ(-1) == LuaType::Nil {
        return state.pop(1).map_err(|e| e.to_string());
    }
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .map_err(|e| format!("verify failed: {e}"))
}

/// Runs the chunk's top level: the scenario's fixture.
fn run_scenario_setup(state: &mut State, source: &[u8], script_path: &Path) -> Result<(), String> {
    let program = Program::compile(source, Some(chunk_name(script_path)))
        .map_err(|e| format!("parse error: {e}"))?;
    state.load(&program).map_err(|e| e.to_string())?;
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .map_err(|e| format!("setup error: {e}"))
}

/// What a scenario's measured loop reports beside its time.
struct ScenarioOutcome {
    measured: Duration,
    extra: Vec<Metric>,
}

#[cfg(feature = "snapshot")]
fn snapshot_save_loop(state: &mut State, count: u32) -> Result<ScenarioOutcome, String> {
    state.gc_collect();
    let start = Instant::now();
    let mut first: Option<Vec<u8>> = None;
    let mut last = Vec::new();
    for _ in 0..count {
        let save = state
            .save_state()
            .map_err(|e| format!("save failed: {e}"))?;
        if first.is_none() {
            first = Some(save.bytes.clone());
        }
        last = save.bytes;
    }
    let measured = start.elapsed();
    if first.as_deref() != Some(last.as_slice()) || last.is_empty() {
        return Err("repeated saves of an unchanged State differ".to_owned());
    }
    Ok(ScenarioOutcome {
        measured,
        extra: vec![integer_metric("save_bytes", last.len() as u128, None)],
    })
}

#[cfg(feature = "snapshot")]
fn snapshot_load_loop(state: &mut State, count: u32) -> Result<ScenarioOutcome, String> {
    state.gc_collect();
    let bytes = state
        .save_state()
        .map_err(|e| format!("save failed: {e}"))?
        .bytes;
    let start = Instant::now();
    let mut restored = None;
    for _ in 0..count {
        let loaded = State::load_state(
            &bytes,
            Box::new(dellingr::DefaultCallbacks),
            |_: &mut State| Ok::<(), std::convert::Infallible>(()),
        )
        .map_err(|e| format!("load failed: {e}"))?;
        // Dropping the previous State here keeps teardown inside the loop.
        restored = Some(loaded);
    }
    let measured = start.elapsed();
    let mut restored = restored.expect("count is at least 1");
    run_verify(&mut restored, None).map_err(|e| format!("restored State: {e}"))?;
    Ok(ScenarioOutcome {
        measured,
        extra: vec![integer_metric("save_bytes", bytes.len() as u128, None)],
    })
}

fn scenario_loop(
    kind: ScenarioKind,
    state: &mut State,
    count: u32,
) -> Result<ScenarioOutcome, String> {
    let plain = |measured| ScenarioOutcome {
        measured,
        extra: Vec::new(),
    };
    match kind {
        ScenarioKind::StateNew => {
            let start = Instant::now();
            for _ in 0..count {
                drop(std::hint::black_box(State::new()));
            }
            let measured = start.elapsed();
            run_verify(state, None)?;
            Ok(plain(measured))
        }
        ScenarioKind::Compile => {
            let source = global_bytes(state, "source")?;
            let start = Instant::now();
            for _ in 0..count {
                let program = Program::compile(&source, Some(b"scenario source"))
                    .map_err(|e| format!("the scenario source does not compile: {e}"))?;
                drop(std::hint::black_box(program));
            }
            let measured = start.elapsed();
            run_verify(state, None)?;
            Ok(ScenarioOutcome {
                measured,
                extra: vec![integer_metric("source_bytes", source.len() as u128, None)],
            })
        }
        ScenarioKind::ManyChunks => {
            let chunk_source = global_bytes(state, "chunk_source")?;
            let chunks = global_count(state, "chunks")?;
            for _ in 0..chunks {
                // A Program compiled on its own is a runtime of its own.
                let program = Program::compile(&chunk_source, Some(b"scenario chunk"))
                    .map_err(|e| format!("chunk_source does not compile: {e}"))?;
                state.load(&program).map_err(|e| e.to_string())?;
                state
                    .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                    .map_err(|e| format!("chunk error: {e}"))?;
            }
            // Settle: free the setup's garbage before the measured loop.
            state.gc_collect();
            let start = Instant::now();
            for _ in 0..count {
                state.gc_collect();
            }
            let measured = start.elapsed();
            run_verify(state, None)?;
            Ok(ScenarioOutcome {
                measured,
                extra: vec![
                    integer_metric("chunks", u128::from(chunks), None),
                    integer_metric("final_object_count", state.object_count() as u128, None),
                ],
            })
        }
        ScenarioKind::InvokeTyped | ScenarioKind::InvokeStack => {
            let step = anchored_function(state, "step")?;
            let height = state.get_top();
            let mut sum = 0.0;
            let start = Instant::now();
            if kind == ScenarioKind::InvokeTyped {
                for i in 0..count {
                    let (r,): (f64,) = state
                        .invoke(&step, (f64::from(i), 0.5))
                        .map_err(|e| format!("step failed: {e}"))?;
                    sum += r;
                }
            } else {
                for i in 0..count {
                    let call = |state: &mut State| -> dellingr::Result<f64> {
                        state.push_number(f64::from(i))?;
                        state.push_number(0.5)?;
                        state.call_anchor(&step, ArgCount::Fixed(2), RetCount::Fixed(1))?;
                        let r = state.to_number(-1)?;
                        state.pop(1)?;
                        Ok(r)
                    };
                    sum += call(state).map_err(|e| format!("step failed: {e}"))?;
                }
            }
            let measured = start.elapsed();
            if state.get_top() != height {
                return Err("the call sequence left the stack at a different height".to_owned());
            }
            run_verify(state, Some(sum))?;
            Ok(plain(measured))
        }
        #[cfg(feature = "snapshot")]
        ScenarioKind::SnapshotSave => snapshot_save_loop(state, count),
        #[cfg(feature = "snapshot")]
        ScenarioKind::SnapshotLoad => snapshot_load_loop(state, count),
        #[cfg(not(feature = "snapshot"))]
        ScenarioKind::SnapshotSave | ScenarioKind::SnapshotLoad => {
            unreachable!("run_scenario refuses snapshot kinds without the snapshot feature")
        }
    }
}

/// The scenario's success line: `host/<file stem>: <count>: true`.
fn scenario_verdict(script_path: &Path, count: u32) -> String {
    let stem = script_path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    format!("host/{stem}: {count}: true")
}

/// Runs a Rust-driven scenario file: its top level (SETUP), then the kind's
/// driver for `count` operations (WARM), then the file's `verify`. Writes
/// its KVs to stderr and its success line to stdout.
fn run_scenario(kind: ScenarioKind, args: &Args, source: &[u8]) -> Result<(), String> {
    if kind.needs_snapshot() && !cfg!(feature = "snapshot") {
        return Err(format!(
            "scenario kind `{}` needs a build with the snapshot feature",
            kind.name()
        ));
    }
    let mut state = State::new();
    if kind == ScenarioKind::ManyChunks {
        // Only the measured explicit collections run, so the cadence of the
        // runtime-cache prune inside the loop follows from `count` alone.
        state.gc_disable_auto();
    }
    emit_marker("SETUP_START");
    let setup_start = Instant::now();
    run_scenario_setup(&mut state, source, &args.script_path)?;
    let count = take_count(&mut state)?;
    let setup_us = setup_start.elapsed().as_micros();
    emit_marker("SETUP_END");

    emit_marker("WARM_START");
    let outcome = scenario_loop(kind, &mut state, count)?;
    emit_marker("WARM_END");

    let measured_us = outcome.measured.as_micros();
    let per_op_ns = outcome.measured.as_secs_f64() * 1e9 / f64::from(count);
    let mut metrics = vec![
        text_metric("target", args.target.clone()),
        text_metric("scenario", kind.name().to_owned()),
        integer_metric("count", u128::from(count), Some(i64::from(count))),
        integer_metric("setup_us", setup_us, None),
        integer_metric("measured_us", measured_us, None),
        text_metric("per_op_ns", format!("{per_op_ns:.1}")),
    ];
    metrics.extend(outcome.extra);
    emit_metrics(&metrics);
    println!("{}", scenario_verdict(&args.script_path, count));
    Ok(())
}

fn main() {
    // Before anything else: the marker clock's origin is process start.
    process_start();

    #[cfg(feature = "hotpath")]
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

    let source = match fs::read(&args.script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", args.script_path.display());
            exit(1);
        }
    };

    match scenario_kind(&source) {
        Ok(None) => {}
        Ok(Some(kind)) => {
            if let Err(error) = run_scenario(kind, &args, &source) {
                eprintln!(
                    "Scenario `{}` failed in {}: {error}",
                    kind.name(),
                    args.script_path.display()
                );
                exit(1);
            }
            return;
        }
        Err(error) => {
            eprintln!("{}: {error}", args.script_path.display());
            exit(1);
        }
    }

    let state_start = Instant::now();
    let mut state = State::new();
    let state_new_us = state_start.elapsed().as_micros();
    if wants_host_records(&source)
        && let Err(e) = install_host_records(&mut state)
    {
        eprintln!("Failed to install host functions: {e}");
        exit(1);
    }

    // Phase 1: parse + codegen.
    emit_marker("PARSE_START");
    let parse_start = Instant::now();
    if let Err(e) = Program::compile(&source, Some(chunk_name(&args.script_path)))
        .and_then(|program| state.load(&program))
    {
        eprintln!("Parse error in {}: {e}", args.script_path.display());
        exit(1);
    }
    let parse_us = parse_start.elapsed().as_micros();
    emit_marker("PARSE_END");

    // Phase 2: run the chunk to register `_bench` (and any per-script setup).
    // This runs the script's whole top level, including its standalone footer
    // (`for i = 1, N do _bench() end`). That footer is deliberate: it is what
    // `brokkr bench` measures. The stored wall is the whole process, so for
    // a seconds-scale bench/ file the footer is most of the verdict wall, and
    // under `brokkr bench --hotpath` the examples/ footer supplies most
    // of the profiled events next to the few warm iterations below. Do not
    // treat it as untimed warm-up overhead to trim.
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
    let block_size = iterations.div_ceil(MAX_WARM_BLOCKS);
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
        warm_total_cost as f64 / f64::from(iterations) / warm_avg_us
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
        warm_total_cost,
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
    use super::{
        RunMetrics, SCENARIO_KINDS, ScenarioKind, chunk_name, find_bench, install_host_records,
        parse_args_from, run_metrics, run_scenario_setup, scenario_kind, scenario_loop, take_count,
        wants_host_records, write_kvs,
    };
    use dellingr::State;
    use std::path::PathBuf;

    #[test]
    fn dellingr_bench_requires_a_script_path() {
        assert_eq!(
            parse_args_from(Vec::<String>::new()).err().as_deref(),
            Some("usage: dellingr-bench <script-path> [--iterations N]")
        );
    }

    #[test]
    fn dellingr_bench_keeps_the_callers_script_path() {
        let args = parse_args_from(["examples/numerics/arithmetic.lua".to_owned()])
            .expect("script path parses");
        assert_eq!(
            args.script_path,
            PathBuf::from("examples/numerics/arithmetic.lua")
        );
    }

    #[test]
    fn dellingr_bench_chunk_name_uses_the_path_without_a_reference_lua_prefix() {
        assert_eq!(
            chunk_name(std::path::Path::new("bench/arithmetic.lua")),
            b"bench/arithmetic.lua"
        );
    }

    #[test]
    fn host_records_directive_must_be_a_whole_line() {
        assert!(wants_host_records(
            b"-- Probe: x\n-- Harness host: records\nlocal a = 1\n"
        ));
        assert!(wants_host_records(b"-- Harness host: records\r\n"));
        assert!(!wants_host_records(b"-- Probe: x\nlocal a = 1\n"));
        assert!(!wants_host_records(b"-- see -- Harness host: records\n"));
    }

    #[test]
    fn host_decode_matches_the_lua_fallback() {
        let mut state = State::new();
        install_host_records(&mut state).expect("host functions install");
        let program = dellingr::Program::compile(
            br#"local e = {x = 1, y = 2, vx = 1, vy = -1, hp = 100, player = 3, kind = "unit", alive = true}
            result = _host_decode(e)"#,
            None,
        )
        .expect("compiles");
        state.load(&program).expect("loads");
        state
            .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
            .expect("runs");
        state.get_global("result").expect("reads the result");
        assert_eq!(state.to_number(-1).expect("a number"), 111.0);
    }

    #[test]
    fn scenario_directive_names_one_known_kind_exactly() {
        assert_eq!(scenario_kind(b"-- Probe: x\nlocal a = 1\n"), Ok(None));
        assert_eq!(
            scenario_kind(b"-- Harness scenario: compile\r\ncount = 1\n"),
            Ok(Some(ScenarioKind::Compile))
        );
        assert!(scenario_kind(b"-- Harness scenario: compile2\n").is_err());
        assert!(scenario_kind(b"-- Harness scenario: compile \n").is_ok());
        assert!(
            scenario_kind(b"-- Harness scenario: compile\n-- Harness scenario: compile\n").is_err()
        );
        for (name, kind) in SCENARIO_KINDS {
            assert_eq!(kind.name(), *name);
        }
    }

    fn run_tiny(kind: ScenarioKind, source: &str) -> Result<(), String> {
        let mut state = State::new();
        if kind == ScenarioKind::ManyChunks {
            state.gc_disable_auto();
        }
        run_scenario_setup(
            &mut state,
            source.as_bytes(),
            std::path::Path::new("tiny.lua"),
        )?;
        let count = take_count(&mut state)?;
        state.get_global("count").map_err(|e| e.to_string())?;
        assert_eq!(state.typ(-1), dellingr::LuaType::Nil, "count is cleared");
        state.pop(1).map_err(|e| e.to_string())?;
        let height = state.get_top();
        scenario_loop(kind, &mut state, count)?;
        assert_eq!(state.get_top(), height, "the driver keeps the stack height");
        Ok(())
    }

    const VERIFY_INVOKE: &str = "count = 3
function step(a, b) return a * 2 + b end
function verify() if result ~= 7.5 then error(\"sum \" .. tostring(result)) end end";

    #[test]
    fn scenario_drivers_run_and_verify() {
        run_tiny(ScenarioKind::StateNew, "count = 2").expect("state_new");
        run_tiny(
            ScenarioKind::Compile,
            "count = 2 source = \"local x = 1 return x\"",
        )
        .expect("compile");
        run_tiny(
            ScenarioKind::ManyChunks,
            "count = 2 chunks = 3 retained = {}
            chunk_source = \"local t = {a = 1} local function f() return t.a end f() retained[#retained + 1] = f\"
            function verify() if #retained ~= 3 then error(\"retained \" .. #retained) end end",
        )
        .expect("many_chunks");
        run_tiny(ScenarioKind::InvokeTyped, VERIFY_INVOKE).expect("invoke_typed");
        run_tiny(ScenarioKind::InvokeStack, VERIFY_INVOKE).expect("invoke_stack");
        assert!(
            run_tiny(
                ScenarioKind::InvokeTyped,
                &VERIFY_INVOKE.replace("7.5", "8")
            )
            .is_err(),
            "a failing verify fails the run"
        );
        assert!(run_tiny(ScenarioKind::StateNew, "count = 0").is_err());
        assert!(run_tiny(ScenarioKind::StateNew, "count = 1.5").is_err());
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_scenario_drivers_run_and_verify() {
        let graph = "count = 2 world = {name = \"w\", list = {1, 2, 3}}
            function verify() if world.list[3] ~= 3 then error(\"lost the graph\") end end";
        run_tiny(ScenarioKind::SnapshotSave, graph).expect("snapshot_save");
        run_tiny(ScenarioKind::SnapshotLoad, graph).expect("snapshot_load");
        assert!(
            run_tiny(ScenarioKind::SnapshotLoad, &graph.replace("~= 3", "~= 4")).is_err(),
            "verify runs on the restored State"
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
            warm_total_cost: 123,
            cost_per_us: 1.12,
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
                "warm_total_cost",
                "cost_per_us",
                "setup_heap_bytes",
                "setup_object_count",
                "final_heap_bytes",
                "final_object_count",
            ]
        );
        assert!(output.contains("warm_avg_cost=6\n"));
        assert!(output.contains("warm_total_cost=123\n"));
        assert!(output.contains("cost_per_us=1.12\n"));
    }
}
