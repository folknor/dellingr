use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct ScratchDirectory {
    path: PathBuf,
}

impl ScratchDirectory {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("dellingr-cli-{name}-{}", std::process::id()));
        fs::create_dir(&path).expect("test directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
            && !std::thread::panicking()
        {
            panic!("test directory should be removed: {error}");
        }
    }
}

struct ScratchScript {
    path: PathBuf,
}

impl ScratchScript {
    fn new(name: &str, source: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("dellingr-cli-{name}-{}.lua", std::process::id()));
        Self::at(path, source)
    }

    fn at(path: PathBuf, source: &str) -> Self {
        let script = Self { path };
        fs::write(&script.path, source).expect("test script should be written");
        script
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchScript {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
            && !std::thread::panicking()
        {
            panic!("test script should be removed: {error}");
        }
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Output {
    run_from(Path::new("."), args)
}

fn run_from(directory: &Path, args: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dellingr"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("dellingr CLI should run")
}

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

    let mut name = b"dellingr-cli-non-utf8-".to_vec();
    name.push(0xff);
    name.extend_from_slice(format!("-{}.lua", std::process::id()).as_bytes());
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(OsString::from_vec(name));
    let script = ScratchScript::at(path, "print('ran')\n");
    let output = run([script.path().as_os_str().to_os_string()]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ran"));
}
