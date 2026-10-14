// The hotpath feature instruments every call in the CLI binary this test
// shells out to, which pushes the whole-corpus run past the gate's per-test
// time budget. Output equivalence says nothing about instrumentation, so the
// differential corpus runs only against the uninstrumented builds.
#![cfg(not(feature = "hotpath"))]

//! Differential testing: compare our VM against lua5.2 and lua5.4
//!
//! This test requires lua5.2 and lua5.4 to be installed on the system.
//! On Ubuntu/Debian: apt install lua5.2 lua5.4

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn lua_versions_available() -> bool {
    let lua52_available = Command::new("lua5.2")
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success());

    let lua54_available = Command::new("lua5.4")
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success());

    lua52_available && lua54_available
}

fn collect_examples(include: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>, include: &impl Fn(&Path) -> bool) {
        for entry in fs::read_dir(dir).expect("examples directory should be readable") {
            let entry = entry.expect("examples directory entry should be readable");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out, include);
            } else if path.extension().is_some_and(|ext| ext == "lua") {
                let rel = path
                    .strip_prefix("examples")
                    .expect("example path should be under examples");
                if include(rel) {
                    out.push(path);
                }
            }
        }
    }

    let mut examples = Vec::new();
    visit(Path::new("examples"), &mut examples, &include);
    examples.sort();
    assert!(!examples.is_empty(), "differential test bucket is empty");
    examples
}

fn run_differential_bucket(name: &str, examples: &[PathBuf]) {
    // Reference Lua is a required dev dependency (scripts/diff_gate.sh fails
    // loudly too); a silent green skip would hide a broken environment.
    assert!(
        lua_versions_available(),
        "lua5.2 and lua5.4 must be on PATH for the differential test \
         (apt install lua5.2 lua5.4)"
    );

    let output = Command::new("./scripts/diff_test.sh")
        .env("DELLINGR", common::dellingr_bin())
        .env("DELLINGR_SKIP_BUILD", "1")
        .env("DELLINGR_SKIP_TIMEOUT", "1")
        .args(examples)
        .output()
        .expect("Failed to run scripts/diff_test.sh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Print the output for visibility in test results
    println!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }

    assert!(
        output.status.success(),
        "Differential test bucket {name} failed!\n\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
}

// The corpus is split so each test stays inside the gate's per-test time
// budget in the unoptimized test profile; together the buckets cover every
// example exactly once (the first two name their directories, the third takes
// the rest).
const KERNEL_DIRS_A: [&str; 5] = ["alloc", "calls", "fields", "globals", "iter"];
const KERNEL_DIRS_B: [&str; 5] = ["numerics", "tables", "strings", "composite", "parse"];

fn top_level_dir(rel: &Path) -> Option<&str> {
    let mut components = rel.components();
    let first = components.next()?.as_os_str().to_str()?;
    components.next().map(|_| first)
}

#[test]
fn differential_kernel_examples_a() {
    run_differential_bucket(
        "kernel-a",
        &collect_examples(|rel| top_level_dir(rel).is_some_and(|dir| KERNEL_DIRS_A.contains(&dir))),
    );
}

#[test]
fn differential_kernel_examples_b() {
    run_differential_bucket(
        "kernel-b",
        &collect_examples(|rel| top_level_dir(rel).is_some_and(|dir| KERNEL_DIRS_B.contains(&dir))),
    );
}

#[test]
fn differential_top_level_examples() {
    run_differential_bucket(
        "top-level",
        &collect_examples(|rel| {
            top_level_dir(rel)
                .is_none_or(|dir| !KERNEL_DIRS_A.contains(&dir) && !KERNEL_DIRS_B.contains(&dir))
        }),
    );
}
