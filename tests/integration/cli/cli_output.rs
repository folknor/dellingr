use std::process::ExitStatus;

use dellingr::CostAnalysis;
use dellingr::Program;
use dellingr::cli::ANALYSIS_WARNING_PREFIX;

use crate::support::process::{ERROR_PREFIX, run_stdin, spawn_stdin};

fn status_with_closed_stdout(args: &[&str], source: &str) -> ExitStatus {
    let mut child = spawn_stdin(args, source.as_bytes());
    drop(child.stdout.take());
    child.wait().expect("CLI should exit")
}

#[test]
fn closed_pipes_do_not_panic_and_error_display_has_one_final_newline() {
    let help_status = status_with_closed_stdout(&["--help"], "");
    assert!(
        help_status.success(),
        "help output should stop quietly on EPIPE"
    );

    let print_status = status_with_closed_stdout(
        &["-"],
        "for i = 1, 10000 do print('xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx') end\n",
    );
    assert!(
        print_status.success(),
        "the CLI print callback should exit quietly on EPIPE"
    );

    let failing = "error('failure')\n";
    let output = run_stdin(["-"], failing);
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

    let mut child = spawn_stdin(["-"], failing.as_bytes());
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
    let output = run_stdin(["--analyze", "-"], "return 1 + 2\n");
    assert!(output.status.success(), "analysis should succeed");

    let stderr = String::from_utf8(output.stderr).expect("CLI diagnostics should be UTF-8");
    assert_eq!(
        stderr,
        format!("{ANALYSIS_WARNING_PREFIX}{}\n", CostAnalysis::caveat())
    );
}

#[test]
fn analyze_report_has_no_extra_blank_line_after_the_display() {
    let source = "return 1 + 2\n";
    let output = run_stdin(["--analyze", "-"], source);
    assert!(output.status.success(), "analysis should succeed");

    let program =
        Program::compile(source, Some(b"stdin".as_slice())).expect("source should compile");
    assert!(output.stdout.ends_with(b"\n"));
    assert!(!output.stdout.ends_with(b"\n\n"));
    assert_eq!(
        output.stdout,
        program.analyze_cost().to_string().as_bytes(),
        "CLI output should match the Display value, including its final newline"
    );
}

#[test]
fn compile_errors_share_context_and_do_not_print_a_cost_summary() {
    let source = "local = \n";
    let run_output = run_stdin(["-"], source);
    let analyze_output = run_stdin(["--analyze", "-"], source);
    let dump_output = run_stdin(["--dump-bytecode", "-"], source);

    for output in [&run_output, &analyze_output, &dump_output] {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("Error: compiling source: "), "{stderr}");
        assert!(!stderr.contains("Cost used:"), "{stderr}");
    }
    assert_eq!(run_output.stderr, analyze_output.stderr);
    assert_eq!(run_output.stderr, dump_output.stderr);
}

#[test]
fn source_size_compile_error_is_not_labeled_as_a_parse_error() {
    let limit = dellingr::limits::MAX_SOURCE_BYTES;
    let source = vec![b' '; limit + 1024];
    let output = run_stdin(["-"], source);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Error: compiling source: stdin: source size "),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("source size {} exceeds limit {limit}", limit + 1)),
        "the CLI should compile only the limit plus one byte: {stderr}"
    );
    assert!(!stderr.contains("parsing source:"), "{stderr}");
    assert!(!stderr.contains("Cost used:"), "{stderr}");
}
