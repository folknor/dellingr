use crate::support::process::{COST_USED_PREFIX, run_stdin};

#[test]
fn invalid_limit_is_rejected_without_running_the_script() {
    let source = "print('ran')\n";
    let output = run_stdin(["--limit", "nope", "-"], source);
    let out_of_range = run_stdin(["--limit", "9223372036854775808", "-"], source);

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
fn missing_limit_value_is_rejected_after_the_input() {
    let output = run_stdin(["-", "--limit"], "print('ran')\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--limit requires a value"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ran"));
}

#[test]
fn signed_limits_are_applied_and_positive_limit_can_run_one_operation() {
    let source = "local x = 0\nx = x + 1\n";

    for limit in ["-1", "0"] {
        let output = run_stdin(["--limit", limit, "-"], source);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("budget of {limit}")));
    }

    let output = run_stdin(["--limit", "1", "-"], source);
    assert!(output.status.success());
    let expected_summary = format!("{COST_USED_PREFIX}1");
    assert!(String::from_utf8_lossy(&output.stderr).contains(&expected_summary));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(COST_USED_PREFIX));
}
