#![allow(clippy::unwrap_used)]
//! Hotpath profiling harness for dellingr.
//!
//! Loads `examples/{target}.lua`, which must define a global `_bench` function.
//! The `target` may include subdirectories (e.g. `calls/global`).
//! Phases measured separately:
//!   - `state_new_us`     time spent creating a default `State`
//!   - `parse_us`         time spent in `load_string_named` (parser + codegen)
//!   - `cold_call_us`     first invocation of `_bench` after chunk setup
//!   - `warm_avg_us`      average of subsequent invocations on the same State
//!
//! KV pairs are emitted to stderr for brokkr to capture into results.db.
//! Determinism: dellingr's cost counter (`cost_used`) is reported alongside
//! wall time; cost-per-microsecond is the stable cross-host metric.
//!
//! Run standalone:
//!     cargo run --release --example hotpath --features hotpath -- numerics/arithmetic

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;

use dellingr::{ArgCount, LuaType, RetCount, State};

const WARM_ITERATIONS: u32 = 20;
const DEFAULT_TARGET: &str = "numerics/arithmetic";

fn main() {
    let _guard = hotpath::HotpathGuardBuilder::new("dellingr::hotpath")
        .percentiles(&[50.0, 95.0, 99.0])
        .functions_limit(0)
        .build();

    let target = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_TARGET.to_string());
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script_path = manifest_dir.join("examples").join(format!("{target}.lua"));

    let source = match fs::read_to_string(&script_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", script_path.display());
            exit(1);
        }
    };

    let state_start = Instant::now();
    let mut state = State::new();
    let state_new_us = state_start.elapsed().as_micros();

    // Phase 1: parse + codegen.
    let chunk_name = format!("@examples/{target}.lua");
    let parse_start = Instant::now();
    if let Err(e) = state.load_string_named(&source, Some(chunk_name)) {
        eprintln!("Parse error in {}: {e}", script_path.display());
        exit(1);
    }
    let parse_us = parse_start.elapsed().as_micros();

    // Phase 2: run the chunk to register `_bench` (and any per-script setup).
    if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        eprintln!("Chunk run error in {}: {e}", script_path.display());
        exit(1);
    }
    let setup_cost = state.cost_used();
    let setup_heap_bytes = state.heap_size();
    let setup_object_count = state.object_count();

    // Phase 3: cold call - first invocation of `_bench`.
    state.get_global("_bench");
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

    let cold_start = Instant::now();
    if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        eprintln!("Cold _bench call error in {}: {e}", script_path.display());
        exit(1);
    }
    let cold_call_us = cold_start.elapsed().as_micros();
    let cold_cost = state.cost_used() - setup_cost;

    // Phase 4: warm calls.
    let warm_start = Instant::now();
    for _ in 0..WARM_ITERATIONS {
        state.get_global("_bench");
        if let Err(e) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
            eprintln!("Warm _bench call error in {}: {e}", script_path.display());
            exit(1);
        }
    }
    let warm_total_us = warm_start.elapsed().as_micros();
    let warm_avg_us = warm_total_us / u128::from(WARM_ITERATIONS);
    let warm_total_cost = state.cost_used() - setup_cost - cold_cost;
    let warm_avg_cost = warm_total_cost / u64::from(WARM_ITERATIONS);

    let cost_per_us = if warm_avg_us > 0 {
        warm_avg_cost as f64 / warm_avg_us as f64
    } else {
        0.0
    };

    let final_heap_bytes = state.heap_size();
    let final_object_count = state.object_count();

    eprintln!("target={target}");
    eprintln!("state_new_us={state_new_us}");
    eprintln!("parse_us={parse_us}");
    eprintln!("cold_call_us={cold_call_us}");
    eprintln!("cold_cost={cold_cost}");
    eprintln!("warm_iterations={WARM_ITERATIONS}");
    eprintln!("warm_avg_us={warm_avg_us}");
    eprintln!("warm_avg_cost={warm_avg_cost}");
    eprintln!("cost_per_us={cost_per_us:.2}");
    eprintln!("setup_heap_bytes={setup_heap_bytes}");
    eprintln!("setup_object_count={setup_object_count}");
    eprintln!("final_heap_bytes={final_heap_bytes}");
    eprintln!("final_object_count={final_object_count}");
}
