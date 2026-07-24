use std::fs;
use std::process::Command;

fn script_path(name: &str, source: &str) -> std::path::PathBuf {
    // CARGO_TARGET_TMPDIR is a cargo-managed temp dir under the project's
    // target/, keeping test scratch out of the system /tmp.
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("dellingr-cli-{name}-{}.lua", std::process::id()));
    fs::write(&path, source).expect("test script should be written");
    path
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dellingr"))
        .args(args)
        .output()
        .expect("dellingr CLI should run")
}

#[test]
fn invalid_limit_is_rejected_without_running_the_script() {
    let script = script_path("invalid", "print('ran')\n");
    let output = run(&[
        "--limit",
        "nope",
        script.to_str().expect("temp path is UTF-8"),
    ]);
    let out_of_range = run(&[
        "--limit",
        "9223372036854775808",
        script.to_str().expect("temp path is UTF-8"),
    ]);
    fs::remove_file(script).expect("test script should be removed");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid --limit value 'nope'"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ran"));
    assert_eq!(out_of_range.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out_of_range.stderr)
            .contains("invalid --limit value '9223372036854775808'")
    );
}

#[test]
fn missing_limit_value_is_rejected_after_a_filename() {
    let script = script_path("missing", "print('ran')\n");
    let output = run(&[script.to_str().expect("temp path is UTF-8"), "--limit"]);
    fs::remove_file(script).expect("test script should be removed");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--limit requires a value"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ran"));
}

#[test]
fn signed_limits_are_applied_and_positive_limit_can_run_one_operation() {
    let script = script_path("signed", "local x = 0\nx = x + 1\n");
    let script = script.to_str().expect("temp path is UTF-8");

    for limit in ["-1", "0"] {
        let output = run(&["--limit", limit, script]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("budget of {limit}")));
    }

    let output = run(&["--limit", "1", script]);
    fs::remove_file(script).expect("test script should be removed");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Cost used: 1"));
}
