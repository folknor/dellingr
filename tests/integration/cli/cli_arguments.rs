//! CLI behaviour driven through stdin. How paths are parsed (dash-named paths,
//! the lone `-`, two positional scripts, non-UTF-8 names) is covered by the
//! unit tests of `parse_args` in `src/main.rs`, which need nothing on disk.

use std::ffi::OsString;

use crate::support::process::{run, run_stdin};

#[test]
fn stdin_scripts_are_named_stdin_and_honour_every_mode() {
    let failed = run_stdin(["-"], "\nerror('boom')\n");
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("stdin:2:"));

    // `-` is stdin after `--` too.
    let terminated = run_stdin(["--", "-"], "print('piped')\n");
    assert!(terminated.status.success());
    assert_eq!(String::from_utf8_lossy(&terminated.stdout), "piped\n");

    let analyzed = run_stdin(["--analyze", "-"], "local x = 1\n");
    assert!(analyzed.status.success());
    assert!(String::from_utf8_lossy(&analyzed.stdout).contains("=== Cost Analysis ==="));

    let source = "print('ran')\n";
    let dumped = run_stdin(["--dump-bytecode", "-"], source);
    assert!(dumped.status.success());
    let program = dellingr::Program::compile(source, Some(b"stdin".as_slice()))
        .expect("source should compile");
    assert_eq!(
        String::from_utf8_lossy(&dumped.stdout),
        program.disassemble().to_string()
    );

    let limited = run_stdin(["--limit", "0", "-"], "local x = 0\nx = x + 1\n");
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("budget of 0"));

    let empty = run_stdin(["--quiet", "-"], "");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.is_empty());

    // Lua source is bytes: a non-UTF-8 literal loads, and `print` writes the
    // script's bytes unchanged.
    let not_utf8 = run_stdin(["--quiet", "-"], b"print('\xff')\n");
    assert!(not_utf8.status.success(), "{not_utf8:?}");
    assert_eq!(not_utf8.stdout, b"\xff\n");
}

#[test]
fn analyze_rejects_limit() {
    let limited = run_stdin(["--limit", "10", "--analyze", "-"], "local x = 1\n");
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("Error: --limit cannot be used"));
    assert!(!String::from_utf8_lossy(&limited.stdout).contains("=== Cost Analysis ==="));
}

#[test]
fn dump_bytecode_lists_the_program_without_running_it() {
    let source = "print('ran', answer)\nlocal function f() return 1 end\n";
    let output = run_stdin(["--dump-bytecode", "-"], source);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("ran\n"), "the script must not execute");
    assert!(stdout.starts_with("function main "));
    assert!(stdout.contains("\nfunction main/0 "));
    assert!(stdout.contains("; \"answer\""));
    assert!(output.stderr.is_empty(), "no cost summary or warning");

    // Same bytes as the library listing for the same source and name.
    let program = dellingr::Program::compile(source, Some(b"stdin".as_slice()))
        .expect("source should compile");
    assert_eq!(stdout, program.disassemble().to_string());

    let quiet = run_stdin(["--quiet", "--dump-bytecode", "-"], source);
    assert!(quiet.status.success());
    assert_eq!(quiet.stdout, output.stdout);

    for conflict in [
        ["--limit", "10", "--dump-bytecode"].as_slice(),
        ["--analyze", "--dump-bytecode"].as_slice(),
    ] {
        let mut args = conflict.to_vec();
        args.push("-");
        let rejected = run_stdin(args, source);
        assert_eq!(rejected.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("--dump-bytecode"));
        assert!(rejected.stdout.is_empty());
    }

    let failed = run_stdin(["--dump-bytecode", "-"], "local = \n");
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).starts_with("Error: "));
    assert!(failed.stdout.is_empty());
}

#[test]
fn help_and_usage_errors_share_the_generated_usage_line() {
    let help = run(["--help"]);
    let missing_file = run(Vec::<OsString>::new());
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    let missing_stderr = String::from_utf8_lossy(&missing_file.stderr);

    assert!(help.status.success());
    assert_eq!(missing_file.status.code(), Some(1));
    assert_eq!(help_stdout.lines().next(), missing_stderr.lines().nth(1));
    assert!(missing_stderr.starts_with("Error: an input file is required\n"));
}
