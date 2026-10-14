//! Integration tests that run all Lua example files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

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

fn run_example(path: &Path) -> std::process::Output {
    Command::new(common::dellingr_bin())
        .arg("--quiet")
        .arg(path)
        .output()
        .expect("Failed to execute test dellingr binary")
}

fn check_example(path: &Path) -> Option<String> {
    let display = path.strip_prefix("examples").unwrap_or(path).display();

    println!("Running: {display}");

    let output = run_example(path);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
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
        } else {
            None
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

    // One test covers the whole tree, so run examples on scoped worker
    // threads to keep gate wall time close to the old per-bucket parallelism.
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(files.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut failures: Vec<(usize, String)> = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = files.get(i) else { break };
                        if let Some(failure) = check_example(path) {
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
