//! Differential testing: compare our VM against lua5.2 and lua5.4
//!
//! This test requires lua5.2 and lua5.4 to be installed on the system.
//! On Ubuntu/Debian: apt install lua5.2 lua5.4

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
                if include(rel) && !should_skip(rel) {
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

fn should_skip(rel: &Path) -> bool {
    let rel = rel
        .to_str()
        .expect("example paths should be valid UTF-8 for diff_test.sh");
    rel == "benchmark.lua"
        || rel == "upvalue_stress.lua"
        || (rel.starts_with("stress_") && !rel.contains('/'))
}

fn top_dir(rel: &Path) -> Option<&str> {
    let mut components = rel.components();
    let first = components.next()?.as_os_str().to_str()?;
    components.next().map(|_| first)
}

fn run_differential_bucket(name: &str, examples: &[PathBuf]) {
    if !lua_versions_available() {
        eprintln!("Skipping differential test: lua5.2 and/or lua5.4 not available");
        eprintln!("Install with: apt install lua5.2 lua5.4");
        return;
    }

    let output = Command::new("./diff_test.sh")
        .args(examples)
        .output()
        .expect("Failed to run diff_test.sh");

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

#[test]
fn differential_root_a_to_feature() {
    run_differential_bucket(
        "root_a_to_feature",
        &collect_examples(|rel| {
            top_dir(rel).is_none() && rel <= Path::new("feature_test_extended.lua")
        }),
    );
}

#[test]
fn differential_root_rest() {
    run_differential_bucket(
        "root_rest",
        &collect_examples(|rel| {
            top_dir(rel).is_none() && rel > Path::new("feature_test_extended.lua")
        }),
    );
}

#[test]
fn differential_alloc_calls_fields_iter() {
    run_differential_bucket(
        "alloc_calls_fields_iter",
        &collect_examples(|rel| {
            matches!(top_dir(rel), Some("alloc" | "calls" | "fields" | "iter"))
        }),
    );
}

#[test]
fn differential_strings() {
    run_differential_bucket(
        "strings",
        &collect_examples(|rel| matches!(top_dir(rel), Some("strings"))),
    );
}

#[test]
fn differential_tables_numerics() {
    run_differential_bucket(
        "tables_numerics",
        &collect_examples(|rel| matches!(top_dir(rel), Some("tables" | "numerics"))),
    );
}
