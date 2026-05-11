//! Integration tests that run all Lua example files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        .unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .unwrap();
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

fn top_dir(path: &Path) -> Option<&str> {
    let rel = path.strip_prefix("examples").unwrap_or(path);
    let mut components = rel.components();
    let first = components.next()?.as_os_str().to_str()?;
    components.next().map(|_| first)
}

fn run_example(path: &Path) -> std::process::Output {
    let release_bin = Path::new("target")
        .join("release")
        .join(format!("dellingr{}", std::env::consts::EXE_SUFFIX));
    if release_bin.exists() {
        Command::new(release_bin)
            .arg("--quiet")
            .arg(path)
            .output()
            .expect("Failed to execute dellingr binary")
    } else if let Some(bin) = std::env::var_os("CARGO_BIN_EXE_dellingr") {
        Command::new(bin)
            .arg("--quiet")
            .arg(path)
            .output()
            .expect("Failed to execute test dellingr binary")
    } else {
        Command::new("cargo")
            .args(["run", "--quiet", "--", "--quiet", path.to_str().unwrap()])
            .output()
            .expect("Failed to execute cargo run")
    }
}

fn check_example(path: &Path) -> Option<String> {
    let display = path.strip_prefix("examples").unwrap_or(path).display();

    println!("Running: {display}");

    let output = run_example(path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(format!(
            "{}: exit code {:?}\nstdout: {}\nstderr: {}",
            display,
            output.status.code(),
            stdout,
            stderr
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

fn run_one_example(path: &str) {
    let path = Path::new(path);
    assert!(path.exists(), "example file not found: {}", path.display());
    if let Some(failure) = check_example(path) {
        panic!("\n1 example failed:\n\n{failure}");
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

    let mut failures = Vec::new();

    for path in &files {
        if let Some(failure) = check_example(path) {
            failures.push(failure);
        }
    }

    if !failures.is_empty() {
        panic!(
            "\n{} example(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }

    println!("\nAll {} examples passed in {name}!", files.len());
}

macro_rules! example_file_test {
    ($name:ident, $path:literal) => {
        #[test]
        fn $name() {
            run_one_example($path);
        }
    };
}

#[test]
fn run_examples_root_a_to_feature() {
    run_examples_bucket("root_a_to_feature", |path| {
        top_dir(path).is_none()
            && path
                .strip_prefix("examples")
                .is_ok_and(|rel| rel <= Path::new("feature_test_extended.lua"))
    });
}

#[test]
fn run_examples_root_rest() {
    run_examples_bucket("root_rest", |path| {
        top_dir(path).is_none()
            && path
                .strip_prefix("examples")
                .is_ok_and(|rel| rel > Path::new("feature_test_extended.lua"))
    });
}

example_file_test!(run_example_alloc_closure, "examples/alloc/closure.lua");
example_file_test!(
    run_example_alloc_record_tables,
    "examples/alloc/record_tables.lua"
);
example_file_test!(
    run_example_alloc_short_tables,
    "examples/alloc/short_tables.lua"
);
example_file_test!(
    run_example_calls_factory_closure,
    "examples/calls/factory_closure.lua"
);
example_file_test!(run_example_calls_fixedarg, "examples/calls/fixedarg.lua");
example_file_test!(run_example_calls_general, "examples/calls/general.lua");
example_file_test!(run_example_calls_global, "examples/calls/global.lua");
example_file_test!(run_example_calls_local, "examples/calls/local.lua");
example_file_test!(run_example_calls_method, "examples/calls/method.lua");
example_file_test!(
    run_example_calls_method_cached,
    "examples/calls/method_cached.lua"
);
example_file_test!(
    run_example_calls_method_chain,
    "examples/calls/method_chain.lua"
);
example_file_test!(run_example_calls_vararg, "examples/calls/vararg.lua");
example_file_test!(
    run_example_fields_polymorphic,
    "examples/fields/polymorphic.lua"
);
example_file_test!(
    run_example_fields_same_obj_cached,
    "examples/fields/same_obj_cached.lua"
);
example_file_test!(
    run_example_fields_same_obj_read,
    "examples/fields/same_obj_read.lua"
);
example_file_test!(
    run_example_fields_same_obj_write,
    "examples/fields/same_obj_write.lua"
);
example_file_test!(run_example_iter_ipairs, "examples/iter/ipairs.lua");
example_file_test!(run_example_iter_pairs, "examples/iter/pairs.lua");
example_file_test!(
    run_example_numerics_arithmetic,
    "examples/numerics/arithmetic.lua"
);
example_file_test!(
    run_example_strings_literal_find,
    "examples/strings/literal_find.lua"
);
example_file_test!(run_example_strings_mixed, "examples/strings/mixed.lua");
example_file_test!(
    run_example_strings_patterns,
    "examples/strings/patterns.lua"
);
example_file_test!(run_example_tables_fill, "examples/tables/fill.lua");
example_file_test!(run_example_tables_mixed, "examples/tables/mixed.lua");
example_file_test!(
    run_example_tables_numeric_index,
    "examples/tables/numeric_index.lua"
);
