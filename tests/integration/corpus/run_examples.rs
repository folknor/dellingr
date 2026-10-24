//! The example corpus gate: one test per example, in `standalone`, expanded
//! from the checked-in list `tests/common/examples.rs`. Each example runs
//! through the CLI once and must pass the example conventions
//! (`check_cli_run`), and the `Cost used: N` it reports must match the corpus
//! cost golden (`check_corpus_cost`); an example not marked `-- SKIP-DIFF:` is then run by
//! reference Lua 5.2 and 5.4 and compared with the CLI's output in memory
//! (`compare_with_references`). `brokkr test standalone::calls_global` runs
//! one example.
//!
//! `brokkr check` runs the standalone tests in their own lane per feature
//! shape: the default and snapshot sweeps skip them, and a sibling sweep of
//! each shape runs only them, several at a time (`parallel` in `brokkr.toml`).
//! They spend their time in child processes, so they overlap well, while the
//! rest of the crate, at-cap memory tests included, stays serial.
//! `standalone_tests_run_in_one_parallel_lane_per_shape` holds `brokkr.toml`
//! to that partition.
//!
//! The comparison is compiled out under the `hotpath` feature: it instruments
//! every call in the CLI binary, and output equivalence says nothing about
//! instrumentation. The pass rules still apply there.

use std::fs;
use std::path::Path;
use std::time::Duration;

use dellingr::{COST_MODEL_VERSION, Program};

use crate::support::corpus_cost_golden as corpus_cost;
#[cfg(not(feature = "hotpath"))]
use crate::support::process::ReferenceLua;
use crate::support::process::{self, Cut, Deadline, End};
use crate::support::{self, examples};

/// The limit on one process's stdout and stderr together.
const OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
/// brokkr's hard per-test wall-time cap (`brokkr man hygiene`). brokkr does
/// not export it, so this restates it, and the assertion below only relates
/// the two local constants.
const BROKKR_PER_TEST_CAP: Duration = Duration::from_secs(20);
/// The processes of one example share this deadline, measured from the start
/// of its test: none starts after it, and one still running at it is killed.
/// A test that passes brokkr's cap stops the whole `brokkr check` run, every
/// other example's verdict with it; a hang inside the deadline is that
/// example's own failure, naming the process. It bounds child execution and
/// output capture, not the in-process compile or a child the kernel will not
/// reap, which brokkr's watchdog still covers. `brokkr test --timeout` raises
/// brokkr's cap but not this deadline.
const EXAMPLE_DEADLINE: Duration = Duration::from_secs(15);
const _: () = assert!(EXAMPLE_DEADLINE.as_secs() + 5 <= BROKKR_PER_TEST_CAP.as_secs());

/// One test per listed example. The module holds the expansion and nothing
/// else, so its path is exactly the selector of the corpus lanes.
mod standalone {
    /// This module's path, from which the lane selector is derived.
    pub(super) const MODULE: &str = module_path!();

    macro_rules! examples {
        ($($name:ident: $path:literal,)*) => {
            $(
                #[test]
                fn $name() {
                    super::run_example($path, concat!(env!("CARGO_MANIFEST_DIR"), "/", $path));
                }
            )*
        };
    }

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/common/examples.rs"
    ));
}

/// The libtest filter that selects exactly the standalone tests.
fn standalone_selector() -> String {
    let (_crate, module) = standalone::MODULE
        .split_once("::")
        .expect("a module path starts with its crate");
    format!("{module}::")
}

fn run_example(relative: &str, path: &str) {
    let deadline = Deadline::after(EXAMPLE_DEADLINE);
    if let Err(failure) = check_example(relative, Path::new(path), deadline) {
        panic!("{failure}");
    }
}

#[test]
fn run_feature_test_extended_in_process() {
    let feature_test = examples::find("examples/feature_test_extended.lua").path;
    let source =
        fs::read_to_string(feature_test).expect("Failed to read feature_test_extended.lua");
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

/// Runs one example through the CLI and, unless it is marked SKIP-DIFF,
/// through both references, and judges it. `display` is its
/// repository-relative path. Every process is started only while the
/// deadline has not passed.
fn check_example(display: &str, path: &Path, deadline: Deadline) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("{display}: could not read source: {error}"))?;
    let comparison = comparison(&source).map_err(|error| format!("{display}: {error}"))?;
    let defines_bench = defines_global_bench(&source);

    // `capture` starts nothing once the deadline has passed. Without `--quiet`
    // the CLI reports `Cost used: N` on stderr, which the corpus cost golden
    // reads.
    let run = process::capture(
        process::limited_command().arg(path),
        OUTPUT_LIMIT_BYTES,
        deadline,
    )
    .map_err(|error| {
        format!(
            "{display}: dellingr (example deadline {}s): {error}",
            EXAMPLE_DEADLINE.as_secs()
        )
    })?;
    let cost = check_cli_run(display, &run, defines_bench)?;
    check_corpus_cost(display, source.as_bytes(), cost)?;
    compare_with_references(path, display, comparison, &run.stdout, deadline)
}

/// The example conventions for the CLI run: it exits 0 and writes nothing to
/// stderr but its one `Cost used: N` line, whose `N` is returned; no stdout
/// line ends in `: false` (examples print `name: <condition>`); and a file
/// whose main chunk assigns a global `_bench` prints a line ending in
/// `: true`.
fn check_cli_run(
    display: &str,
    run: &process::Captured,
    defines_bench: bool,
) -> Result<u64, String> {
    let status = match &run.end {
        End::Exited(status) => status,
        End::Cut(cut) => return Err(cut_failure(display, "dellingr", cut, run)),
    };
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let cost = stderr
        .strip_prefix("Cost used: ")
        .and_then(|rest| rest.strip_suffix('\n'))
        .map(str::parse::<u64>);
    if !status.success() {
        return Err(format!(
            "{display}: exit code {:?}\nstdout: {stdout}\nstderr: {stderr}",
            status.code()
        ));
    }
    let Some(Ok(cost)) = cost else {
        return Err(format!(
            "{display}: stderr should be exactly one `Cost used: N` line\nstdout: {stdout}\nstderr: {stderr}"
        ));
    };
    if stdout.lines().any(|line| line.ends_with(": false")) {
        return Err(format!(
            "{display}: test assertion failed\noutput: {stdout}"
        ));
    }
    if defines_bench && !stdout.lines().any(|line| line.ends_with(": true")) {
        return Err(format!(
            "{display}: _bench example did not print a success line\noutput: {stdout}"
        ));
    }
    Ok(cost)
}

/// The recorded corpus costs (`tests/common/corpus_cost_golden.rs` describes the
/// format and its verdicts).
const CORPUS_COST: &str = include_str!("../../fixtures/corpus_cost.txt");

fn corpus_cost_fixture() -> corpus_cost::Fixture {
    corpus_cost::parse(CORPUS_COST)
        .unwrap_or_else(|error| panic!("tests/fixtures/corpus_cost.txt: {error}"))
}

/// The corpus cost golden: the CLI's `cost_used` for an example matches the
/// row recorded for its source.
fn check_corpus_cost(display: &str, source: &[u8], cost: u64) -> Result<(), String> {
    corpus_cost_fixture().judge(
        COST_MODEL_VERSION,
        display,
        corpus_cost::source_digest(source),
        cost,
    )
}

/// The fixture has exactly one row per listed example, in list order, and is
/// in the form the regeneration tool writes, so a hand edit or a stale row
/// fails here rather than lingering.
#[test]
fn corpus_cost_fixture_rows_are_the_example_list() {
    let fixture = corpus_cost_fixture();
    let recorded: Vec<&str> = fixture.rows.iter().map(|row| row.path.as_str()).collect();
    let listed: Vec<&str> = examples::EXAMPLES
        .iter()
        .map(|example| example.relative)
        .collect();
    assert_eq!(
        recorded, listed,
        "tests/fixtures/corpus_cost.txt must list every example once, in list order; \
         regenerate it with `brokkr run --release regenerate_corpus_cost -- tests/fixtures/corpus_cost.txt`"
    );
    assert_eq!(
        corpus_cost::render(&fixture),
        CORPUS_COST,
        "tests/fixtures/corpus_cost.txt is not in the regeneration tool's form"
    );
}

/// The regeneration tool refuses to record a new cost for unchanged source
/// under an unchanged cost model, and allows it for changed source or after a
/// version bump.
#[test]
fn corpus_cost_regeneration_refuses_unversioned_drift() {
    let fixture = corpus_cost_fixture();
    let row = &fixture.rows[0];
    let replacement = |cost_model, source, cost| corpus_cost::Fixture {
        cost_model,
        rows: vec![corpus_cost::Row {
            path: row.path.clone(),
            source,
            cost,
        }],
    };
    let model = fixture.cost_model;
    assert!(
        fixture
            .unversioned_changes(&replacement(model, row.source, row.cost))
            .is_empty()
    );
    assert_eq!(
        fixture
            .unversioned_changes(&replacement(model, row.source, row.cost + 1))
            .len(),
        1
    );
    assert!(
        fixture
            .unversioned_changes(&replacement(model, row.source ^ 1, row.cost + 1))
            .is_empty()
    );
    assert!(
        fixture
            .unversioned_changes(&replacement(model + 1, row.source, row.cost + 1))
            .is_empty()
    );
}

/// The failure of a process whose capture was cut short.
fn cut_failure(display: &str, program: &str, cut: &Cut, run: &process::Captured) -> String {
    let output: String = [("stdout", &run.stdout), ("stderr", &run.stderr)]
        .into_iter()
        .filter(|(_, bytes)| !bytes.is_empty())
        .map(|(name, bytes)| format!("\n{name}: {}", String::from_utf8_lossy(bytes)))
        .collect();
    let deadline = EXAMPLE_DEADLINE.as_secs();
    match cut {
        Cut::OutputExceeded => format!(
            "{display}: {program}: stdout and stderr exceeded the combined output limit of {OUTPUT_LIMIT_BYTES} bytes"
        ),
        Cut::Unfinished => {
            format!(
                "{display}: {program} was still running at the {deadline}s example deadline{output}"
            )
        }
        Cut::CaptureUnfinished(status) => format!(
            "{display}: {program} exited ({status}), but its output had not been read to the end \
             by the {deadline}s example deadline{output}"
        ),
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
) -> Result<(), String> {
    if matches!(comparison, Comparison::Skipped) {
        return Ok(());
    }
    let mut matched = Vec::new();
    let mut mismatches = Vec::new();
    for lua in ReferenceLua::ALL {
        let mut command =
            process::reference_command(lua, path).map_err(|error| format!("{display}: {error}"))?;
        // A start failure is usually a missing interpreter, so name the
        // executable. `capture` starts nothing once the deadline has passed.
        let run =
            process::capture(&mut command, OUTPUT_LIMIT_BYTES, deadline).map_err(|error| {
                format!(
                    "{display}: {} ({}, example deadline {}s, dellingr passed): {error}",
                    lua.label(),
                    lua.program(),
                    EXAMPLE_DEADLINE.as_secs()
                )
            })?;
        let status = match &run.end {
            End::Exited(status) => *status,
            End::Cut(cut) => return Err(cut_failure(display, lua.label(), cut, &run)),
        };
        let Some(code) = status.code() else {
            return Err(format!(
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
        Comparison::Compared if matched.is_empty() => Err(format!(
            "{display}: output matches neither reference\n{}",
            mismatches.join("\n")
        )),
        Comparison::Diverges if !matched.is_empty() => Err(format!(
            "{display}: stale DIFF marker; output matches {}",
            matched.join(" and ")
        )),
        _ => Ok(()),
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
) -> Result<(), String> {
    Ok(())
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

/// A string list in a `[[check]]` entry, if the key is present.
fn check_list<'a>(check: &'a toml::Table, key: &str) -> Option<Vec<&'a str>> {
    check.get(key).map(|value| {
        value
            .as_array()
            .unwrap_or_else(|| panic!("brokkr.toml [[check]] {key} should be a list"))
            .iter()
            .map(|item| {
                item.as_str()
                    .unwrap_or_else(|| panic!("brokkr.toml [[check]] {key} should hold strings"))
            })
            .collect()
    })
}

/// What decides which build a `[[check]]` entry tests: its features (as a
/// set), defaults, profile, target selection and feature unification as
/// written. brokkr's `auto` (an absent key) promotes a parallel sweep of the
/// whole workspace to `workspace` under `brokkr check` but not under
/// `brokkr test` or `-p`, so only the same written mode builds the same thing
/// everywhere.
fn build_shape(check: &toml::Table) -> String {
    let mut features = check_list(check, "features").unwrap_or_default();
    features.sort_unstable();
    let unification = check
        .get("feature_unification")
        .and_then(toml::Value::as_str)
        .unwrap_or("auto");
    format!(
        "features {features:?}, unification {unification}, no_default_features {:?}, \
         profile {:?}, tests {:?}, lib_tests {:?}",
        check
            .get("no_default_features")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        check.get("profile").and_then(toml::Value::as_str),
        check_list(check, "tests"),
        check.get("lib_tests").and_then(toml::Value::as_bool),
    )
}

/// `brokkr check` runs the standalone tests once per feature shape, in a lane
/// of their own (see the module comment). Every sweep that runs the
/// integration crate without an `only` filter skips them; each such sweep has
/// exactly one sibling of the same build that runs only them, in brokkr's
/// shared-process parallel lane; a build may also have a standalone lane
/// without such a parent (the release corpus), but never two lanes; and no
/// other sweep names or selects them.
/// The selector comes from the module's own path, so renaming the module
/// fails here rather than moving the corpus into the serial lane or out of
/// both. brokkr refuses a sweep in which no test ran, which covers an
/// expansion that produced no tests.
#[test]
fn standalone_tests_run_in_one_parallel_lane_per_shape() {
    let selector = standalone_selector();
    let standalone_tests: Vec<String> = examples::EXAMPLES
        .iter()
        .map(|example| format!("{selector}{}", example.name))
        .collect();
    let config = support::brokkr_config();
    let checks = config
        .get("check")
        .and_then(toml::Value::as_array)
        .expect("brokkr.toml should list check shapes");

    let mut parents = Vec::new();
    let mut siblings = Vec::new();
    for check in checks {
        let check = check
            .as_table()
            .expect("brokkr.toml [[check]] entries are tables");
        let name = check
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("brokkr.toml [[check]] entries have names");
        let skip = check_list(check, "skip").unwrap_or_default();
        let only = check_list(check, "only");
        let doc_only = check.get("doc_only").and_then(toml::Value::as_bool) == Some(true);
        // Without `tests`, an entry runs every harness, unless `lib_tests`
        // narrows it to the library's.
        let runs_integration = !doc_only
            && match check_list(check, "tests") {
                Some(tests) => tests.contains(&"integration"),
                None => check.get("lib_tests").and_then(toml::Value::as_bool) != Some(true),
            };

        if only.as_deref() == Some(&[selector.as_str()]) {
            assert!(
                runs_integration,
                "[[check]] {name} selects the standalone tests but does not run the integration crate"
            );
            assert!(
                check.get("parallel").is_some_and(toml::Value::is_table),
                "[[check]] {name} runs the standalone tests and needs `parallel = {{}}`"
            );
            assert!(
                check
                    .get("harness")
                    .is_none_or(|harness| harness.as_str() == Some("libtest")),
                "[[check]] {name} must run the standalone tests in the shared-process libtest lane"
            );
            assert!(
                skip.is_empty(),
                "[[check]] {name} runs the standalone tests and skips nothing"
            );
            assert!(
                check
                    .get("feature_unification")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|mode| mode != "auto"),
                "[[check]] {name} must pin feature_unification, as its main sweep does: under \
                 auto, brokkr check promotes the parallel lane to workspace unification"
            );
            siblings.push((name, build_shape(check)));
            continue;
        }
        let selects_standalone = |filter: &str| {
            standalone_tests
                .iter()
                .any(|test| test.contains(filter) || filter.contains(selector.as_str()))
        };
        if runs_integration && only.is_none() {
            assert!(
                skip.contains(&selector.as_str()),
                "[[check]] {name} runs the integration crate and must skip {selector:?}, \
                 which its standalone sibling runs"
            );
            // The rest of the crate, at-cap memory tests included, runs one
            // test at a time in one process.
            assert!(
                check.get("parallel").is_none()
                    && check
                        .get("harness")
                        .is_none_or(|harness| harness.as_str() == Some("libtest")),
                "[[check]] {name} runs the integration crate and must stay in the serial libtest lane"
            );
            parents.push((name, build_shape(check)));
        } else {
            assert!(
                !skip.iter().any(|filter| selects_standalone(filter)),
                "[[check]] {name} names the standalone tests in skip"
            );
        }
        if runs_integration && let Some(only) = &only {
            assert!(
                !only.iter().any(|filter| selects_standalone(filter)),
                "[[check]] {name} selects standalone tests; only their own lanes may"
            );
        }
    }

    assert!(!parents.is_empty(), "no sweep runs the integration crate");
    let profiles = config
        .get("test")
        .and_then(|test| test.get("profiles"))
        .and_then(toml::Value::as_table);
    // A profile's key, as brokkr resolves it through `extends` (child wins).
    let resolved = |profile: &str, key: &str| -> Option<toml::Value> {
        let profiles = profiles?;
        let mut current = profile.to_owned();
        let mut seen = Vec::new();
        loop {
            let settings = profiles.get(&current)?.as_table()?;
            if let Some(value) = settings.get(key) {
                return Some(value.clone());
            }
            seen.push(current.clone());
            current = settings.get("extends")?.as_str()?.to_owned();
            if seen.contains(&current) {
                return None;
            }
        }
    };
    for profile in profiles.into_iter().flat_map(toml::Table::keys) {
        let sweeps_value = resolved(profile, "sweeps");
        let sweeps = sweeps_value
            .as_ref()
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        let threads = resolved(profile, "test_threads").and_then(|value| value.as_integer());
        assert!(
            threads.is_none_or(|threads| threads == 1)
                || !parents.iter().any(|(parent, _)| sweeps.contains(parent)),
            "profile {profile} runs a serial integration sweep with test_threads {threads:?}"
        );
    }
    for (parent, shape) in &parents {
        let matching: Vec<&str> = siblings
            .iter()
            .filter(|(_, sibling_shape)| sibling_shape == shape)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "[[check]] {parent} ({shape}) needs exactly one standalone sibling of the same build, found {matching:?}"
        );
    }
    // A build with no full serial sweep (the release corpus) still gets one
    // standalone lane at most, so no build runs the corpus twice.
    for (sibling, shape) in &siblings {
        assert!(
            parents
                .iter()
                .filter(|(_, parent_shape)| parent_shape == shape)
                .count()
                <= 1,
            "standalone sweep {sibling} ({shape}) has more than one sweep of the same build that skips it"
        );
        assert_eq!(
            siblings
                .iter()
                .filter(|(_, other_shape)| other_shape == shape)
                .count(),
            1,
            "standalone sweep {sibling} ({shape}) shares its build with another standalone sweep"
        );
    }
}
