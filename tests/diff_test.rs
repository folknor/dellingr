//! Differential corpus checks and focused checks for the gate's input contracts.
//!
//! This is the differential gate: `brokkr check` runs it once per `[[check]]`
//! feature shape in `brokkr.toml`, so the corpus is checked against the CLI
//! build of that shape. Each script process is bounded by the 5s
//! `DELLINGR_DIFF_TIMEOUT` passed to `scripts/diff_test.sh`.
//!
//! The corpus tests are compiled out under the `hotpath` feature: it
//! instruments every call in the CLI binary, which pushes the corpus past the
//! per-test time budget, and output equivalence says nothing about
//! instrumentation. The input-contract tests still run there.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(not(feature = "hotpath"))]
mod common;

fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn malformed_timeout_is_rejected_once_before_other_checks() {
    let output = Command::new("bash")
        .arg(manifest_path("scripts/diff_test.sh"))
        .env("DELLINGR_DIFF_TIMEOUT", "5 s")
        .output()
        .expect("bash should be available to check timeout validation");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "malformed timeout should be a startup refusal; stderr: {stderr}"
    );
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        vec!["invalid DELLINGR_DIFF_TIMEOUT: 5 s"],
        "malformed timeout should produce one refusal line"
    );
    assert!(
        output.stdout.is_empty(),
        "startup refusal should not print to stdout"
    );
}

fn collect_empty_reason_markers(dir: &Path, root: &Path, empty: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("examples directory should be readable") {
        let path = entry
            .expect("examples directory entry should be readable")
            .path();
        if path.is_dir() {
            collect_empty_reason_markers(&path, root, empty);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("lua") {
            continue;
        }

        let source = fs::read_to_string(&path).expect("example source should be readable");
        for (line_number, line) in source.lines().enumerate() {
            let marker = line
                .strip_prefix("-- DIFF:")
                .map(|reason| ("DIFF", reason))
                .or_else(|| {
                    line.strip_prefix("-- SKIP-DIFF:")
                        .map(|reason| ("SKIP-DIFF", reason))
                });
            match marker {
                Some((kind, reason)) if reason.trim().is_empty() => {
                    let relative_path = path
                        .strip_prefix(root)
                        .expect("example should be under the examples directory");
                    empty.push(format!(
                        "{}:{} {kind} marker",
                        relative_path.display(),
                        line_number + 1
                    ));
                }
                _ => {}
            }
        }
    }
}

#[test]
fn diff_markers_have_nonempty_reasons() {
    let examples = manifest_path("examples");
    let mut empty = Vec::new();
    collect_empty_reason_markers(&examples, &examples, &mut empty);
    empty.sort();

    assert!(
        empty.is_empty(),
        "DIFF and SKIP-DIFF markers need a reason: {empty:?}"
    );
}

#[cfg(not(feature = "hotpath"))]
fn collect_examples(include: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    fn visit(dir: &Path, root: &Path, out: &mut Vec<PathBuf>, include: &impl Fn(&Path) -> bool) {
        for entry in fs::read_dir(dir).expect("examples directory should be readable") {
            let entry = entry.expect("examples directory entry should be readable");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, root, out, include);
            } else if path.extension().is_some_and(|ext| ext == "lua") {
                let rel = path
                    .strip_prefix(root)
                    .expect("example path should be under examples");
                if include(rel) {
                    out.push(path);
                }
            }
        }
    }

    let root = manifest_path("examples");
    let mut examples = Vec::new();
    visit(&root, &root, &mut examples, &include);
    examples.sort();
    assert!(!examples.is_empty(), "differential test bucket is empty");
    examples
}

#[cfg(not(feature = "hotpath"))]
fn run_differential_bucket(name: &str, examples: &[PathBuf]) {
    // diff_test.sh refuses to compare without both reference Luas (named by
    // LUA52 and LUA54, defaulting to lua5.2 and lua5.4) and says which one is
    // missing, so a host without them fails here rather than skipping.
    let output = Command::new("bash")
        .arg(manifest_path("scripts/diff_test.sh"))
        .env("DELLINGR", common::dellingr_bin())
        .env("DELLINGR_SKIP_BUILD", "1")
        .env("DELLINGR_DIFF_TIMEOUT", "5s")
        .args(examples)
        .output()
        .expect("bash should be available to run scripts/diff_test.sh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }

    assert!(
        output.status.success(),
        "Differential test bucket {name} failed!\n\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
}

#[cfg(not(feature = "hotpath"))]
// Keep each integration test within the test-profile time budget while covering
// every example exactly once per feature shape.
const KERNEL_DIRS_A: [&str; 5] = ["alloc", "calls", "fields", "globals", "iter"];
#[cfg(not(feature = "hotpath"))]
const KERNEL_DIRS_B: [&str; 5] = ["numerics", "tables", "strings", "composite", "parse"];

#[cfg(not(feature = "hotpath"))]
fn top_level_dir(rel: &Path) -> Option<&str> {
    let mut components = rel.components();
    let first = components.next()?.as_os_str().to_str()?;
    components.next().map(|_| first)
}

/// Kernel examples in sorted order, dealt round-robin into `KERNEL_CHUNKS`
/// buckets. Each kernel script runs for about a second in the unoptimized test
/// profile, so the buckets stay small enough for the per-test time budget.
#[cfg(not(feature = "hotpath"))]
const KERNEL_CHUNKS: usize = 6;

#[cfg(not(feature = "hotpath"))]
fn kernel_chunk(chunk: usize) -> Vec<PathBuf> {
    let all = collect_examples(|rel| {
        top_level_dir(rel)
            .is_some_and(|dir| KERNEL_DIRS_A.contains(&dir) || KERNEL_DIRS_B.contains(&dir))
    });
    all.into_iter()
        .enumerate()
        .filter(|(index, _)| index % KERNEL_CHUNKS == chunk)
        .map(|(_, path)| path)
        .collect()
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_0() {
    run_differential_bucket("kernel-0", &kernel_chunk(0));
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_1() {
    run_differential_bucket("kernel-1", &kernel_chunk(1));
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_2() {
    run_differential_bucket("kernel-2", &kernel_chunk(2));
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_3() {
    run_differential_bucket("kernel-3", &kernel_chunk(3));
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_4() {
    run_differential_bucket("kernel-4", &kernel_chunk(4));
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_kernel_examples_5() {
    run_differential_bucket("kernel-5", &kernel_chunk(5));
}

#[cfg(not(feature = "hotpath"))]
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
