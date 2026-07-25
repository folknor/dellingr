#![allow(clippy::unwrap_used)]
//! Bench harness for dellingr: the binary brokkr invokes for `--bench` /
//! `--hotpath` / `--alloc` runs, and the KV source for scripts/hotbench.sh.
//!
//! Loads a bench script that must define a global `_bench` function, then
//! runs phases on one `State`:
//!   - parse   `load_string_named` (parser + codegen)
//!   - setup   top-level chunk run (registers `_bench`, builds fixtures)
//!   - cold    first `_bench` invocation
//!   - warm    N further invocations on the same State (`--iterations N`,
//!     default 20)
//!
//! The script may be given as a path (`examples/numerics/arithmetic.lua`) or
//! as a bare target name (`numerics/arithmetic`) resolved against the
//! crate's `examples/` directory. brokkr passes the registered file path;
//! hotbench.sh and humans may use either.
//!
//! Output:
//!   - KV pairs on stderr - phase timings and heap/object brackets.
//!   - Optionally, the brokkr sidecar stream: when `BROKKR_MARKER_FIFO`
//!     names a writable path, phase markers (`PARSE_START`/`PARSE_END`,
//!     `SETUP_*`, `COLD_*`, `WARM_*`, plus batched `WARM_BLOCK_*` sub-spans)
//!     and `@name=value` counters are written to it. Without the env var
//!     every emit is a no-op; a full pipe drops writes silently
//!     (O_NONBLOCK).
//!
//! Wall time is the benchmarking metric. The cost KVs ride along as a
//! workload fingerprint - cost is a deterministic property of the script,
//! so a cost change between runs of the "same" bench means the workload or
//! the compiler changed, not the VM's speed.
//!
//! Run standalone:
//!     cargo run --release --example hotpath -- numerics/arithmetic
//!     cargo run --release --example hotpath --features hotpath -- numerics/arithmetic

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;

use dellingr::{ArgCount, LuaType, RetCount, State};

const DEFAULT_ITERATIONS: u32 = 20;
const DEFAULT_TARGET: &str = "numerics/arithmetic";
/// Upper bound on `WARM_BLOCK` marker pairs per run; keeps the sidecar
/// stream at tens of spans regardless of the iteration count.
const MAX_WARM_BLOCKS: u32 = 32;

/// Emit a named phase marker to the brokkr sidecar (if active).
///
/// Protocol: `<timestamp_us> <name>\n` on the FIFO named by
/// `BROKKR_MARKER_FIFO`, timestamped in microseconds since the first emit.
fn emit_marker(name: &str) {
    use std::io::Write;
    write_fifo(|f, us| {
        drop((&*f).write_all(format!("{us} {name}\n").as_bytes()));
    });
}

/// Emit a named counter value to the brokkr sidecar (if active).
///
/// Protocol: `<timestamp_us> @<name>=<value>\n`.
fn emit_counter(name: &str, value: i64) {
    use std::io::Write;
    write_fifo(|f, us| {
        drop((&*f).write_all(format!("{us} @{name}={value}\n").as_bytes()));
    });
}

/// Shared FIFO write logic for markers and counters. No-op when
/// `BROKKR_MARKER_FIFO` is unset or the path cannot be opened.
fn write_fifo(f: impl FnOnce(&std::fs::File, u128)) {
    use std::sync::OnceLock;

    static STATE: OnceLock<Option<(std::fs::File, Instant)>> = OnceLock::new();

    let state = STATE.get_or_init(|| {
        let Ok(path) = env::var("BROKKR_MARKER_FIFO") else {
            return None;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            #[cfg(target_os = "linux")]
            const O_NONBLOCK: i32 = 0x800;
            #[cfg(target_os = "macos")]
            const O_NONBLOCK: i32 = 0x0004;
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(O_NONBLOCK)
                .open(&path)
            {
                Ok(file) => Some((file, Instant::now())),
                Err(_) => None,
            }
        }
        #[cfg(not(unix))]
        {
            let _unused = path;
            None
        }
    });

    if let Some((file, start)) = state.as_ref() {
        let us = start.elapsed().as_micros();
        f(file, us);
    }
}

fn counter_value(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

struct Args {
    script_path: PathBuf,
    /// Display name for the `target=` KV: the bare target for legacy
    /// invocations, the given path otherwise.
    target: String,
    iterations: u32,
}

fn parse_args() -> Args {
    let mut positional: Option<String> = None;
    let mut iterations = DEFAULT_ITERATIONS;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iterations" {
            match args.next().map(|v| v.parse::<u32>()) {
                Some(Ok(n)) if n > 0 => iterations = n,
                _ => {
                    eprintln!("--iterations requires a positive integer");
                    exit(2);
                }
            }
        } else if positional.is_none() {
            positional = Some(arg);
        } else {
            eprintln!("unexpected argument: {arg}");
            exit(2);
        }
    }

    let raw = positional.unwrap_or_else(|| DEFAULT_TARGET.to_string());
    if raw.ends_with(".lua") {
        Args {
            script_path: PathBuf::from(&raw),
            target: raw,
            iterations,
        }
    } else {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Args {
            script_path: manifest_dir.join("examples").join(format!("{raw}.lua")),
            target: raw,
            iterations,
        }
    }
}

fn main() {
    let _guard = hotpath::HotpathGuardBuilder::new("dellingr::hotpath")
        .percentiles(&[50.0, 95.0, 99.0])
        .functions_limit(0)
        .build();

    let args = parse_args();
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
    emit_counter("setup_heap_bytes", counter_value(setup_heap_bytes as u64));
    emit_counter(
        "setup_object_count",
        counter_value(setup_object_count as u64),
    );

    // Phase 3: cold call - first invocation of `_bench`.
    state.get_global("_bench").unwrap();
    if state.typ(-1) == LuaType::Nil {
        state
            .pop(1)
            .expect("_bench function lookup leaves one stack value");
        let final_heap_bytes = state.heap_size();
        let final_object_count = state.object_count();

        eprintln!("target={target}");
        eprintln!("state_new_us={state_new_us}");
        eprintln!("parse_us={parse_us}");
        eprintln!("setup_cost={setup_cost}");
        eprintln!("setup_heap_bytes={setup_heap_bytes}");
        eprintln!("setup_object_count={setup_object_count}");
        eprintln!("final_heap_bytes={final_heap_bytes}");
        eprintln!("final_object_count={final_object_count}");
        return;
    }

    emit_marker("COLD_START");
    let cold_start = Instant::now();
    if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        eprintln!(
            "Cold _bench call error in {}: {e}",
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
    let block_size = (iterations / MAX_WARM_BLOCKS).max(1);
    emit_marker("WARM_START");
    let warm_start = Instant::now();
    let mut done = 0u32;
    while done < iterations {
        let n = block_size.min(iterations - done);
        emit_marker("WARM_BLOCK_START");
        for _ in 0..n {
            state.get_global("_bench").unwrap();
            if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
                eprintln!(
                    "Warm _bench call error in {}: {e}",
                    args.script_path.display()
                );
                exit(1);
            }
        }
        emit_marker("WARM_BLOCK_END");
        done += n;
    }
    let warm_total_us = warm_start.elapsed().as_micros();
    emit_marker("WARM_END");
    let warm_avg_us = warm_total_us / u128::from(iterations);
    let warm_total_cost = state.cost_used() - setup_cost - cold_cost;
    let warm_avg_cost = warm_total_cost / u64::from(iterations);

    let cost_per_us = if warm_avg_us > 0 {
        warm_avg_cost as f64 / warm_avg_us as f64
    } else {
        0.0
    };

    let final_heap_bytes = state.heap_size();
    let final_object_count = state.object_count();
    emit_counter("warm_iterations", i64::from(iterations));
    emit_counter("warm_avg_cost", counter_value(warm_avg_cost));
    emit_counter("final_heap_bytes", counter_value(final_heap_bytes as u64));
    emit_counter(
        "final_object_count",
        counter_value(final_object_count as u64),
    );

    eprintln!("target={target}");
    eprintln!("state_new_us={state_new_us}");
    eprintln!("parse_us={parse_us}");
    eprintln!("cold_call_us={cold_call_us}");
    eprintln!("cold_cost={cold_cost}");
    eprintln!("warm_iterations={iterations}");
    eprintln!("warm_avg_us={warm_avg_us}");
    eprintln!("warm_avg_cost={warm_avg_cost}");
    eprintln!("cost_per_us={cost_per_us:.2}");
    eprintln!("setup_heap_bytes={setup_heap_bytes}");
    eprintln!("setup_object_count={setup_object_count}");
    eprintln!("final_heap_bytes={final_heap_bytes}");
    eprintln!("final_object_count={final_object_count}");
}
