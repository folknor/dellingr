//! Differential corpus checks and focused checks for the gate's input contracts.
//!
//! This is the differential gate: `brokkr check` runs it once per `[[check]]`
//! feature shape in `brokkr.toml`, so the corpus is checked against the CLI
//! build of that shape. Each script process is bounded by the default
//! per-process timeout of `scripts/diff_test.sh` (set in `scripts/lib.sh`).
//!
//! The corpus tests are compiled out under the `hotpath` feature: it
//! instruments every call in the CLI binary, which pushes the corpus past the
//! per-test time budget, and output equivalence says nothing about
//! instrumentation. The input-contract tests still run there.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(feature = "hotpath"))]
use std::process::Stdio;

use crate::support::examples;

fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn malformed_timeout_is_rejected_once_before_other_checks() {
    let output = Command::new("bash")
        .arg(manifest_path("scripts/diff_test.sh"))
        .arg("--bin")
        // Timeout validation should fail before the non-executable path is checked.
        .arg("/dev/null")
        .args(["--timeout", "5 s"])
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
        vec!["invalid --timeout: 5 s"],
        "malformed timeout should produce one refusal line"
    );
    assert!(
        output.stdout.is_empty(),
        "startup refusal should not print to stdout"
    );
}

fn collect_empty_reason_markers(paths: &[PathBuf], root: &Path, empty: &mut Vec<String>) {
    for path in paths {
        let source = fs::read_to_string(path).expect("example source should be readable");
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
    let corpus = examples::example_corpus();
    collect_empty_reason_markers(&corpus, &examples, &mut empty);
    empty.sort();

    assert!(
        empty.is_empty(),
        "DIFF and SKIP-DIFF markers need a reason: {empty:?}"
    );
}

#[cfg(not(feature = "hotpath"))]
fn collect_examples(include: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    // The shared corpus is manifest-rooted; filters receive examples-relative paths.
    let root = manifest_path("examples");
    let examples: Vec<PathBuf> = examples::example_corpus()
        .into_iter()
        .filter(|path| {
            let relative = path
                .strip_prefix(&root)
                .expect("example corpus paths must be under the examples directory");
            include(relative)
        })
        .collect();
    assert!(!examples.is_empty(), "differential test bucket is empty");
    examples
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn shell_and_rust_example_corpora_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg(root.join("scripts/diff_test.sh"))
        .arg("--list-examples")
        .current_dir(root)
        .output()
        .expect("bash should be available to list the shell example corpus");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "shell corpus listing failed: {stderr}"
    );
    assert!(
        stderr.is_empty(),
        "shell corpus listing wrote stderr: {stderr}"
    );

    let examples_dir = root.join("examples");
    let rust_paths: Vec<Vec<u8>> = examples::example_corpus()
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("example corpus paths must be rooted at the crate manifest")
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        })
        .collect();
    assert!(!rust_paths.is_empty(), "example corpus must not be empty");
    let shell_paths: Vec<Vec<u8>> = output
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(&output.stdout)
        .split(|byte| *byte == b'\n')
        .filter(|_| !output.stdout.is_empty())
        .map(<[u8]>::to_vec)
        .collect();

    assert_eq!(
        shell_paths,
        rust_paths,
        "Rust example list differs from scripts/diff_test.sh under {}",
        examples_dir.display()
    );
}

/// Kernel examples run for about a second each in the unoptimized test
/// profile, so they are dealt round-robin into this many partitions. The
/// remaining examples are fast and share one more partition.
#[cfg(not(feature = "hotpath"))]
const KERNEL_PARTITIONS: usize = 6;

#[cfg(not(feature = "hotpath"))]
fn is_kernel_example(rel: &Path) -> bool {
    // A kernel example lives in a subdirectory; top-level examples are fast.
    rel.components().count() > 1
}

/// The whole corpus, every example exactly once, split into partitions that
/// `diff_test.sh` runs concurrently. brokkr runs test binaries with one test
/// thread, so separate tests would run these one after another.
#[cfg(not(feature = "hotpath"))]
#[test]
fn differential_examples() {
    let mut partitions: Vec<Vec<PathBuf>> = vec![Vec::new(); KERNEL_PARTITIONS];
    let kernels = collect_examples(is_kernel_example);
    for (index, path) in kernels.into_iter().enumerate() {
        partitions[index % KERNEL_PARTITIONS].push(path);
    }
    partitions.push(collect_examples(|rel| !is_kernel_example(rel)));

    // diff_test.sh requires both lua5.2 and lua5.4 and says which executable
    // is missing, so a host without them fails here rather than skipping.
    let children: Vec<_> = partitions
        .iter()
        .map(|examples| {
            Command::new("bash")
                .arg(manifest_path("scripts/diff_test.sh"))
                .arg("--bin")
                .arg(crate::support::dellingr_bin())
                .arg("--")
                .args(examples)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("bash should be available to run scripts/diff_test.sh")
        })
        .collect();

    let mut failures = Vec::new();
    for (index, child) in children.into_iter().enumerate() {
        let output = child
            .wait_with_output()
            .expect("diff_test.sh partition output should be readable");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            failures.push(format!(
                "partition {index} failed\n\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Differential test failed!\n\n{}",
        failures.join("\n\n")
    );
}
