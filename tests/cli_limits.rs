#[path = "common/cli.rs"]
mod cli;
mod common;
#[path = "common/scratch.rs"]
mod scratch;

use cli::run;
use dellingr::cli::COST_USED_PREFIX;
use scratch::ScratchScript;

#[test]
fn invalid_limit_is_rejected_without_running_the_script() {
    let script = ScratchScript::new("invalid", "print('ran')\n");
    let script_path = script.path().to_str().expect("temp path is UTF-8");
    let output = run(["--limit", "nope", script_path]);
    let out_of_range = run(["--limit", "9223372036854775808", script_path]);

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
    let script = ScratchScript::new("missing", "print('ran')\n");
    let output = run([
        script.path().to_str().expect("temp path is UTF-8"),
        "--limit",
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--limit requires a value"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ran"));
}

#[test]
fn signed_limits_are_applied_and_positive_limit_can_run_one_operation() {
    let script = ScratchScript::new("signed", "local x = 0\nx = x + 1\n");
    let script_path = script.path().to_str().expect("temp path is UTF-8");

    for limit in ["-1", "0"] {
        let output = run(["--limit", limit, script_path]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("budget of {limit}")));
    }

    let output = run(["--limit", "1", script_path]);
    assert!(output.status.success());
    let expected_summary = format!("{COST_USED_PREFIX}1");
    assert!(String::from_utf8_lossy(&output.stderr).contains(&expected_summary));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(COST_USED_PREFIX));
}
