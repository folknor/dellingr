#[path = "common/cli.rs"]
mod cli;
mod common;
#[path = "common/scratch.rs"]
mod scratch;

use std::ffi::OsStr;
use std::process::{ExitStatus, Stdio};

use cli::{command, run};
use scratch::ScratchScript;

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
        stderr.contains("Error: "),
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
