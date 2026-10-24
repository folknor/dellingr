use std::ffi::OsStr;
use std::process::{ExitStatus, Stdio};

use dellingr::CostAnalysis;
use dellingr::cli::ANALYSIS_WARNING_PREFIX;

use crate::support::process::{ERROR_PREFIX, command, run};
use crate::support::scratch::ScratchScript;

fn status_with_closed_stdout(argument: &OsStr) -> ExitStatus {
    let mut child = command()
        .arg(argument)
        .stdout(Stdio::piped())
        .spawn()
        .expect("CLI should start");
    drop(child.stdout.take());
    child.wait().expect("CLI should exit")
}

#[test]
fn closed_pipes_do_not_panic_and_error_display_has_one_final_newline() {
    let help_status = status_with_closed_stdout(OsStr::new("--help"));
    assert!(
        help_status.success(),
        "help output should stop quietly on EPIPE"
    );

    let printing = ScratchScript::new(
        "output-print",
        "for i = 1, 10000 do print('xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx') end\n",
    );
    let print_status = status_with_closed_stdout(printing.path().as_os_str());
    assert!(
        print_status.success(),
        "the CLI print callback should exit quietly on EPIPE"
    );

    let failing = ScratchScript::new("output-error", "error('failure')\n");
    let output = run([failing.path()]);
    let status = output.status;
    let stderr = String::from_utf8(output.stderr).expect("CLI diagnostics should be UTF-8");
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains(ERROR_PREFIX),
        "expected runtime error in stderr"
    );
    assert!(
        stderr.ends_with('\n'),
        "CLI should end its diagnostic with one newline"
    );
    assert!(
        !stderr.ends_with("\n\n"),
        "Error display should not add an extra blank line"
    );

    let mut child = command()
        .arg(failing.path())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start with a closed stderr pipe");
    drop(child.stderr.take());
    let closed_stderr_status = child.wait().expect("CLI should exit");
    assert_eq!(
        closed_stderr_status.code(),
        Some(1),
        "a closed stderr pipe should not replace the runtime error status with a panic"
    );
}

#[test]
fn analyze_warning_uses_the_library_caveat() {
    let script = ScratchScript::new("output-analysis", "return 1 + 2\n");
    let output = run([OsStr::new("--analyze"), script.path().as_os_str()]);
    assert!(output.status.success(), "analysis should succeed");

    let stderr = String::from_utf8(output.stderr).expect("CLI diagnostics should be UTF-8");
    assert_eq!(
        stderr,
        format!("{ANALYSIS_WARNING_PREFIX}{}\n", CostAnalysis::caveat())
    );
}

#[test]
fn parse_errors_share_context_and_do_not_print_a_cost_summary() {
    let script = ScratchScript::new("output-parse-error", "local = \n");
    let path = script.path().as_os_str();
    let run_output = run([path]);
    let analyze_output = run([OsStr::new("--analyze"), path]);
    let dump_output = run([OsStr::new("--dump-bytecode"), path]);

    for output in [&run_output, &analyze_output, &dump_output] {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("Error: parsing source: "), "{stderr}");
        assert!(!stderr.contains("Cost used:"), "{stderr}");
    }
    assert_eq!(run_output.stderr, analyze_output.stderr);
    assert_eq!(run_output.stderr, dump_output.stderr);
}
