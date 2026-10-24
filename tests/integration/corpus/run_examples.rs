//! The example corpus gate. Every Lua file under `examples/` runs through the
//! CLI once and must pass the example conventions (`check_cli_run`); every
//! file not marked `-- SKIP-DIFF:` is then run by reference Lua 5.2 and 5.4
//! and compared with the CLI's output in memory (`compare_with_references`).
//! `brokkr check` runs it in every sweep whose selection includes it.
//!
//! `check_example` depends on one path and a deadline, so the corpus can
//! become one test per example. Until then `run_all_examples` runs the whole
//! corpus in one test, under one deadline below brokkr's per-test cap, and
//! names the examples still running or not started when it passes.
//!
//! The comparison is compiled out under the `hotpath` feature: it instruments
//! every call in the CLI binary, and output equivalence says nothing about
//! instrumentation. The pass rules still apply there.

use std::fs;
use std::path::Path;
use std::time::Duration;

use dellingr::Program;

#[cfg(not(feature = "hotpath"))]
use crate::support::process::ReferenceLua;
use crate::support::process::{self, Cut, Deadline, End};
use crate::support::{self, examples};

const EXAMPLE_WORKERS: usize = 8;
/// The limit on one process's stdout and stderr together.
const OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
/// brokkr's hard per-test wall-time cap (`brokkr man hygiene`). brokkr does
/// not export it, so this restates it.
const BROKKR_PER_TEST_CAP: Duration = Duration::from_secs(20);
/// The whole corpus shares this deadline, measured from the start of the
/// test. It sits far enough inside the cap that a slow or hung example is
/// reported by name, with the rest of the corpus's verdicts, instead of being
/// killed by brokkr's watchdog with the whole test as the suspect.
const CORPUS_DEADLINE: Duration = Duration::from_secs(15);
const _: () = assert!(CORPUS_DEADLINE.as_secs() + 5 <= BROKKR_PER_TEST_CAP.as_secs());

#[test]
fn run_feature_test_extended_in_process() {
    let feature_test = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("feature_test_extended.lua");
    let source =
        fs::read_to_string(&feature_test).expect("Failed to read feature_test_extended.lua");
    let (mut state, recorder) = support::Recorder::state();
    support::run(&mut state, &source);

    let prints = recorder.prints();
    let failure_lines = prints
        .iter()
        .filter(|line| line.starts_with(b"[FAIL]"))
        .map(|line| String::from_utf8_lossy(line))
        .collect::<Vec<_>>();
    assert!(
        failure_lines.is_empty(),
        "feature_test_extended.lua reported failures: {failure_lines:?}"
    );

    let expected_passes = source
        .lines()
        .filter(|line| line.trim_start().starts_with("test("))
        .count();
    let actual_passes = prints
        .iter()
        .filter(|line| line.starts_with(b"[PASS]"))
        .count();
    assert!(expected_passes > 0, "the example must contain test cases");
    assert_eq!(
        actual_passes, expected_passes,
        "feature_test_extended.lua must report every test case as passing"
    );
    assert!(
        prints
            .iter()
            .any(|line| line.as_slice() == b"\n=== All extended tests passed! ==="),
        "feature_test_extended.lua must print its completion marker"
    );
}

pub(super) fn defines_global_bench(source: &str) -> bool {
    if !source.contains("_bench") {
        return false;
    }

    let Ok(program) = Program::compile(source, None) else {
        return false;
    };
    let listing = program.disassemble().to_string();
    let root = listing
        .split_once("\nfunction main/")
        .map_or(listing.as_str(), |(root, _)| root);

    // Root bytecode writes distinguish a global bench from a local function.
    root.lines().any(|line| {
        let Some((instruction, annotation)) = line.split_once("  ; ") else {
            return false;
        };
        let writes_global = instruction
            .split_whitespace()
            .any(|part| part == "SET_GLOBAL");
        let global_name = annotation
            .split_once(", ")
            .map_or(annotation, |(name, _)| name);
        writes_global && global_name == "\"_bench\""
    })
}

/// How an example takes part in the reference comparison, from its marker
/// lines: `-- DIFF: <reason>` declares a deliberate divergence, and
/// `-- SKIP-DIFF: <reason>` keeps the file out of the comparison.
#[derive(Clone, Copy)]
enum Comparison {
    /// Unmarked: the output must match at least one reference.
    Compared,
    /// `-- DIFF:`: the output must match neither reference; a match means
    /// the marker is stale.
    Diverges,
    /// `-- SKIP-DIFF:`: not compared.
    Skipped,
}

/// Reads the markers of `source`. Every marker needs a reason; whitespace is
/// `str::trim_ascii`'s set, the same one the `diff-marker-needs-reason`
/// textlint rule in `brokkr.toml` uses, which reports the same mistake before
/// anything compiles. A file may not carry both kinds.
fn comparison(source: &str) -> Result<Comparison, String> {
    let mut diverges = false;
    let mut skipped = false;
    let mut without_reason = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let reason = if let Some(reason) = line.strip_prefix("-- DIFF:") {
            diverges = true;
            reason
        } else if let Some(reason) = line.strip_prefix("-- SKIP-DIFF:") {
            skipped = true;
            reason
        } else {
            continue;
        };
        if reason.trim_ascii().is_empty() {
            without_reason.push((index + 1).to_string());
        }
    }
    if !without_reason.is_empty() {
        return Err(format!(
            "DIFF and SKIP-DIFF markers need a reason (line {})",
            without_reason.join(", ")
        ));
    }
    match (diverges, skipped) {
        (true, true) => Err("carries both a DIFF and a SKIP-DIFF marker".to_string()),
        (true, false) => Ok(Comparison::Diverges),
        (false, true) => Ok(Comparison::Skipped),
        (false, false) => Ok(Comparison::Compared),
    }
}

/// The verdict on one example.
enum Outcome {
    Passed,
    Failed(String),
    /// The deadline passed while the example was running; the text says which
    /// process it was in.
    Unfinished(String),
    /// The deadline had passed before the example started.
    NotStarted,
}

/// Runs one example through the CLI and, unless it is marked SKIP-DIFF,
/// through both references, and judges it. Every process is started only
/// while the deadline has not passed.
fn check_example(path: &Path, deadline: Deadline) -> Outcome {
    if deadline.passed() {
        return Outcome::NotStarted;
    }
    let display = display_path(path);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => return Outcome::Failed(format!("{display}: could not read source: {error}")),
    };
    let comparison = match comparison(&source) {
        Ok(comparison) => comparison,
        Err(error) => return Outcome::Failed(format!("{display}: {error}")),
    };
    let defines_bench = defines_global_bench(&source);

    if deadline.passed() {
        return Outcome::NotStarted;
    }
    let run = match process::capture(
        process::limited_command().arg("--quiet").arg(path),
        OUTPUT_LIMIT_BYTES,
        deadline,
    ) {
        Ok(run) => run,
        Err(error) => return Outcome::Failed(format!("{display}: dellingr: {error}")),
    };
    if let Some(failure) = check_cli_run(&display, &run, defines_bench) {
        return failure;
    }
    compare_with_references(path, &display, comparison, &run.stdout, deadline)
}

/// The example conventions for the CLI run: it exits 0 and writes nothing to
/// stderr; no stdout line ends in `: false` (examples print
/// `name: <condition>`); and a file whose main chunk assigns a global `_bench`
/// prints a line ending in `: true`.
fn check_cli_run(display: &str, run: &process::Captured, defines_bench: bool) -> Option<Outcome> {
    let status = match &run.end {
        End::Exited(status) => status,
        End::Cut(cut) => return Some(cut_outcome(display, "dellingr", cut, run)),
    };
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let failure = if !status.success() {
        format!(
            "{display}: exit code {:?}\nstdout: {stdout}\nstderr: {stderr}",
            status.code()
        )
    } else if !stderr.is_empty() {
        format!("{display}: unexpected stderr\nstdout: {stdout}\nstderr: {stderr}")
    } else if stdout.lines().any(|line| line.ends_with(": false")) {
        format!("{display}: test assertion failed\noutput: {stdout}")
    } else if defines_bench && !stdout.lines().any(|line| line.ends_with(": true")) {
        format!("{display}: _bench example did not print a success line\noutput: {stdout}")
    } else {
        return None;
    };
    Some(Outcome::Failed(failure))
}

/// The verdict on a process whose capture was cut short.
fn cut_outcome(display: &str, program: &str, cut: &Cut, run: &process::Captured) -> Outcome {
    let output: String = [("stdout", &run.stdout), ("stderr", &run.stderr)]
        .into_iter()
        .filter(|(_, bytes)| !bytes.is_empty())
        .map(|(name, bytes)| format!("\n{name}: {}", String::from_utf8_lossy(bytes)))
        .collect();
    match cut {
        Cut::OutputExceeded => Outcome::Failed(format!(
            "{display}: {program}: stdout and stderr exceeded the combined output limit of {OUTPUT_LIMIT_BYTES} bytes"
        )),
        Cut::Unfinished => {
            Outcome::Unfinished(format!("{display}: {program} was still running{output}"))
        }
        Cut::CaptureUnfinished(status) => Outcome::Unfinished(format!(
            "{display}: {program} exited ({status}), but its output had not been read to the end{output}"
        )),
    }
}

/// Compares the CLI's stdout with both references, run under the conditions
/// `process::reference_command` documents.
///
/// A reference matches when it exits 0, writes nothing to stderr and prints
/// exactly the CLI's stdout. The CLI has already passed with exit 0 and empty
/// stderr, so this compares everything both processes produced, stream by
/// stream. A reference that exits nonzero with an error message is ordinary
/// divergence (Lua 5.4 rejects a zero for-loop step that dellingr skips, for
/// instance). A reference that cannot be judged - it could not start, was
/// killed by a signal, overflowed its output or was cut by the deadline -
/// fails the example whatever the other reference did, so broken reference
/// infrastructure can neither validate a DIFF marker nor hide behind the
/// other reference's match.
///
/// Both references are accepted because dellingr is a Lua 5.2/5.4 hybrid
/// (the README's "Won't implement" list removes most of what separates the
/// two), and where they disagree, which one dellingr follows is a per-feature
/// decision rather than a global one. A file where both agree cannot hide a
/// regression, and a deliberate divergence is marked with `-- DIFF:`.
#[cfg(not(feature = "hotpath"))]
fn compare_with_references(
    path: &Path,
    display: &str,
    comparison: Comparison,
    ours: &[u8],
    deadline: Deadline,
) -> Outcome {
    if matches!(comparison, Comparison::Skipped) {
        return Outcome::Passed;
    }
    let mut matched = Vec::new();
    let mut mismatches = Vec::new();
    for lua in ReferenceLua::ALL {
        if deadline.passed() {
            return Outcome::Unfinished(format!(
                "{display}: {} had not started (dellingr passed)",
                lua.label()
            ));
        }
        let mut command = match process::reference_command(lua, path) {
            Ok(command) => command,
            Err(error) => return Outcome::Failed(format!("{display}: {error}")),
        };
        let run = match process::capture(&mut command, OUTPUT_LIMIT_BYTES, deadline) {
            Ok(run) => run,
            // A start failure is usually a missing interpreter, so name the
            // executable.
            Err(error) => {
                return Outcome::Failed(format!(
                    "{display}: {} ({}): {error}",
                    lua.label(),
                    lua.program()
                ));
            }
        };
        let status = match &run.end {
            End::Exited(status) => *status,
            End::Cut(cut) => return cut_outcome(display, lua.label(), cut, &run),
        };
        let Some(code) = status.code() else {
            return Outcome::Failed(format!(
                "{display}: {} did not exit normally ({status})",
                lua.label()
            ));
        };
        if code == 0 && run.stderr.is_empty() && run.stdout == ours {
            matched.push(lua.label());
        } else {
            mismatches.push(describe_mismatch(lua.label(), code, &run, ours));
        }
    }
    match comparison {
        Comparison::Compared if matched.is_empty() => Outcome::Failed(format!(
            "{display}: output matches neither reference\n{}",
            mismatches.join("\n")
        )),
        Comparison::Diverges if !matched.is_empty() => Outcome::Failed(format!(
            "{display}: stale DIFF marker; output matches {}",
            matched.join(" and ")
        )),
        _ => Outcome::Passed,
    }
}

/// Without the comparison (see the module comment), the pass rules decide.
#[cfg(feature = "hotpath")]
fn compare_with_references(
    _path: &Path,
    _display: &str,
    _comparison: Comparison,
    _ours: &[u8],
    _deadline: Deadline,
) -> Outcome {
    Outcome::Passed
}

/// How a reference's run differs from the CLI's: its exit code, its stderr
/// and the first stdout line that differs.
#[cfg(not(feature = "hotpath"))]
fn describe_mismatch(label: &str, code: i32, run: &process::Captured, ours: &[u8]) -> String {
    let mut text = format!("  {label}: exit code {code}");
    if !run.stderr.is_empty() {
        text.push_str(&format!(
            ", stderr: {}",
            String::from_utf8_lossy(&run.stderr).trim_end()
        ));
    }
    if run.stdout != ours {
        let ours: Vec<&[u8]> = ours.split(|byte| *byte == b'\n').collect();
        let theirs: Vec<&[u8]> = run.stdout.split(|byte| *byte == b'\n').collect();
        let line = (0..ours.len().max(theirs.len()))
            .find(|&index| ours.get(index) != theirs.get(index))
            .unwrap_or(0);
        let show = |lines: &[&[u8]]| {
            lines.get(line).map_or_else(
                || "<end of output>".to_string(),
                |text| format!("{:?}", String::from_utf8_lossy(text)),
            )
        };
        text.push_str(&format!(
            "\n    first difference at stdout line {}\n    dellingr: {}\n    {label}: {}",
            line + 1,
            show(&ours),
            show(&theirs)
        ));
    }
    text
}

fn display_path(path: &Path) -> String {
    path.strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("example corpus paths must be rooted at the crate manifest")
        .display()
        .to_string()
}

/// The whole corpus in one test; see the module comment.
#[test]
fn run_all_examples() {
    let deadline = Deadline::after(CORPUS_DEADLINE);
    let files = examples::example_corpus();
    assert!(!files.is_empty(), "no .lua files found under examples/");

    let outcomes = support::parallel_map(&files, EXAMPLE_WORKERS, |path| {
        check_example(path, deadline)
    });

    let mut failures = Vec::new();
    let mut unfinished = Vec::new();
    let mut not_started = Vec::new();
    for (path, outcome) in files.iter().zip(outcomes) {
        match outcome {
            Outcome::Passed => {}
            Outcome::Failed(failure) => failures.push(failure),
            Outcome::Unfinished(report) => unfinished.push(report),
            Outcome::NotStarted => not_started.push(display_path(path)),
        }
    }
    if !unfinished.is_empty() || !not_started.is_empty() {
        failures.push(format!(
            "the corpus deadline of {}s (brokkr's per-test cap is {}s) passed with {} example(s) \
             unfinished and {} not started\n\nunfinished:\n{}\n\nnot started:\n{}",
            CORPUS_DEADLINE.as_secs(),
            BROKKR_PER_TEST_CAP.as_secs(),
            unfinished.len(),
            not_started.len(),
            unfinished.join("\n\n"),
            not_started.join("\n")
        ));
    }
    assert!(
        failures.is_empty(),
        "\n{} problem(s) in the {}-example corpus:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}
