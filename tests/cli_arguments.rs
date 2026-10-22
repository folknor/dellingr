#[path = "common/cli.rs"]
mod cli;
mod common;
#[path = "common/scratch.rs"]
mod scratch;

use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use std::process::Output;

use cli::{command, run, run_from};
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

/// Runs the CLI in `directory` with `input` as its stdin, fed from a scratch file.
fn run_with_stdin(directory: &Path, args: &[&str], input: &[u8]) -> Output {
    let stdin = ScratchScript::at(directory.join("stdin-input"), input);
    let output = command()
        .current_dir(directory)
        .args(args)
        .stdin(File::open(stdin.path()).expect("stdin input should open"))
        .output()
        .expect("dellingr CLI should run");
    drop(stdin);
    output
}

#[test]
fn terminator_allows_dash_paths_and_a_lone_dash_reads_stdin() {
    let directory = ScratchDirectory::new("dash-paths");
    let named_script = ScratchScript::at(directory.path().join("-script.lua"), "print('named')\n");
    let lone_dash_script = ScratchScript::at(directory.path().join("-"), "print('file')\n");

    let without_terminator = run_from(directory.path(), strings(&["-script.lua"]));
    assert_eq!(without_terminator.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&without_terminator.stderr).contains("Error: unknown option"));

    let with_terminator = run_from(directory.path(), strings(&["--", "-script.lua"]));
    assert!(with_terminator.status.success());
    assert!(String::from_utf8_lossy(&with_terminator.stdout).contains("named"));

    // `-` is stdin even beside a file named `-` and after `--`; `./-` names the file.
    for args in [["-"].as_slice(), ["--", "-"].as_slice()] {
        let lone_dash = run_with_stdin(directory.path(), args, b"print('piped')\n");
        assert!(lone_dash.status.success(), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&lone_dash.stdout), "piped\n");
    }
    let dash_file = run_from(directory.path(), strings(&["./-"]));
    assert!(dash_file.status.success());
    assert_eq!(String::from_utf8_lossy(&dash_file.stdout), "file\n");

    drop(lone_dash_script);
    drop(named_script);
}

#[test]
fn stdin_scripts_are_named_stdin_and_honour_every_mode() {
    let directory = ScratchDirectory::new("stdin-modes");

    let failed = run_with_stdin(directory.path(), &["-"], b"\nerror('boom')\n");
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("stdin:2:"));

    let analyzed = run_with_stdin(directory.path(), &["--analyze", "-"], b"local x = 1\n");
    assert!(analyzed.status.success());
    assert!(String::from_utf8_lossy(&analyzed.stdout).contains("=== Cost Analysis ==="));

    let source = "print('ran')\n";
    let dumped = run_with_stdin(
        directory.path(),
        &["--dump-bytecode", "-"],
        source.as_bytes(),
    );
    assert!(dumped.status.success());
    let program = dellingr::Engine::new()
        .compile_named(source, "stdin")
        .expect("source should compile");
    assert_eq!(
        String::from_utf8_lossy(&dumped.stdout),
        program.disassemble().to_string()
    );

    let limited = run_with_stdin(
        directory.path(),
        &["--limit", "0", "-"],
        b"local x = 0\nx = x + 1\n",
    );
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("budget of 0"));

    let empty = run_with_stdin(directory.path(), &["--quiet", "-"], b"");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.is_empty());

    let not_utf8 = run_with_stdin(directory.path(), &["-"], b"print('\xff')\n");
    assert_eq!(not_utf8.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&not_utf8.stderr).starts_with("Error: reading stdin: "));
}

#[test]
fn analyze_rejects_limit_and_sends_its_warning_to_stderr() {
    let script = ScratchScript::new("analyze", "local x = 1\n");
    let script_path = script.path().to_str().expect("temp path is UTF-8");
    let output = run(strings(&["--analyze", script_path]));

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("=== Cost Analysis ==="));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Warning:"));
    let expected_warning = format!(
        "{}{}",
        dellingr::cli::ANALYSIS_WARNING_PREFIX,
        dellingr::CostAnalysis::caveat()
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(&expected_warning));

    let limited = run(strings(&["--limit", "10", "--analyze", script_path]));
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("Error: --limit cannot be used"));
    assert!(!String::from_utf8_lossy(&limited.stdout).contains("=== Cost Analysis ==="));
}

#[test]
fn dump_bytecode_lists_the_program_without_running_it() {
    let source = "print('ran', answer)\nlocal function f() return 1 end\n";
    let script = ScratchScript::new("dump", source);
    let script_path = script.path().to_str().expect("temp path is UTF-8");
    let output = run(strings(&["--dump-bytecode", script_path]));

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("ran\n"), "the script must not execute");
    assert!(stdout.starts_with("function main "));
    assert!(stdout.contains("\nfunction main/0 "));
    assert!(stdout.contains("; \"answer\""));
    assert!(output.stderr.is_empty(), "no cost summary or warning");

    // Same bytes as the library listing for the same source and name.
    let program = dellingr::Engine::new()
        .compile_named(source, script_path)
        .expect("source should compile");
    assert_eq!(stdout, program.disassemble().to_string());

    let quiet = run(strings(&["--quiet", "--dump-bytecode", script_path]));
    assert!(quiet.status.success());
    assert_eq!(quiet.stdout, output.stdout);

    for conflict in [
        ["--limit", "10", "--dump-bytecode"].as_slice(),
        ["--analyze", "--dump-bytecode"].as_slice(),
    ] {
        let mut args = conflict.to_vec();
        args.push(script_path);
        let rejected = run(strings(&args));
        assert_eq!(rejected.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("--dump-bytecode"));
        assert!(rejected.stdout.is_empty());
    }

    let broken = ScratchScript::new("dump-broken", "local = \n");
    let broken_path = broken.path().to_str().expect("temp path is UTF-8");
    let failed = run(strings(&["--dump-bytecode", broken_path]));
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).starts_with("Error: "));
    assert!(failed.stdout.is_empty());
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
