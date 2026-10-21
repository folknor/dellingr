#[path = "common/cli.rs"]
mod cli;
mod common;
#[path = "common/scratch.rs"]
mod scratch;

use std::ffi::OsString;

use cli::{run, run_from};
use scratch::{ScratchDirectory, ScratchScript};

fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn rejects_multiple_script_paths_instead_of_running_the_last() {
    let first = ScratchScript::new("first", "print('first ran')\n");
    let second = ScratchScript::new("second", "print('second ran')\n");
    let output = run([
        first.path().as_os_str().to_os_string(),
        second.path().as_os_str().to_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Error: only one input file"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ran"));
}

#[test]
fn terminator_allows_dash_paths_and_a_lone_dash_is_a_path() {
    let directory = ScratchDirectory::new("dash-paths");
    let named_script = ScratchScript::at(directory.path().join("-script.lua"), "print('named')\n");
    let lone_dash_script = ScratchScript::at(directory.path().join("-"), "print('dash')\n");

    let without_terminator = run_from(directory.path(), strings(&["-script.lua"]));
    assert_eq!(without_terminator.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&without_terminator.stderr).contains("Error: unknown option"));

    let with_terminator = run_from(directory.path(), strings(&["--", "-script.lua"]));
    assert!(with_terminator.status.success());
    assert!(String::from_utf8_lossy(&with_terminator.stdout).contains("named"));

    let lone_dash = run_from(directory.path(), strings(&["-"]));
    assert!(lone_dash.status.success());
    assert!(String::from_utf8_lossy(&lone_dash.stdout).contains("dash"));

    drop(lone_dash_script);
    drop(named_script);
}

#[test]
fn analyze_rejects_limit_and_sends_its_warning_to_stderr() {
    let script = ScratchScript::new("analyze", "local x = 1\n");
    let script_path = script.path().to_str().expect("temp path is UTF-8");
    let output = run(strings(&["--analyze", script_path]));

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("=== Cost Analysis ==="));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Warning:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Warning: analyze_cost"));

    let limited = run(strings(&["--limit", "10", "--analyze", script_path]));
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("Error: --limit cannot be used"));
    assert!(!String::from_utf8_lossy(&limited.stdout).contains("=== Cost Analysis ==="));
}

#[test]
fn help_and_usage_errors_share_the_generated_usage_line() {
    let help = run(strings(&["--help"]));
    let missing_file = run(Vec::<OsString>::new());
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    let missing_stderr = String::from_utf8_lossy(&missing_file.stderr);

    assert!(help.status.success());
    assert_eq!(missing_file.status.code(), Some(1));
    assert_eq!(help_stdout.lines().next(), missing_stderr.lines().nth(1));
    assert!(missing_stderr.starts_with("Error: an input file is required\n"));
}

#[cfg(unix)]
#[test]
fn accepts_non_utf8_script_paths() {
    use std::os::unix::ffi::OsStringExt;

    let directory = ScratchDirectory::new("non-utf8");
    let path = directory
        .path()
        .join(OsString::from_vec(b"script-\xff.lua".to_vec()));
    let script = ScratchScript::at(path, "print('ran')\n");
    let output = run([script.path().as_os_str().to_os_string()]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ran"));
}
