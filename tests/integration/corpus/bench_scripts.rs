use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use dellingr::{ArgCount, LuaType, Program, RetCount, ScopeCost, State};
use xxhash_rust::xxh3::xxh3_128;

use crate::support;

const BENCH_SETUP_COST_BUDGET: i64 = 2_000_000;
const BENCH_KERNEL_COST_BUDGET: i64 = 40_000_000;
const BENCH_SCRIPT_WORKERS: usize = 8;

// The generated parse probe has no separate verdict-side benchmark file.
const SAME_FILE_WORKLOAD_ALLOWLIST: &[(&str, &str)] =
    &[("large_source", "examples/parse/large_source.lua")];

// Keep the expected function name tied to the constant used by the harness.
const HOTPATH_SOURCE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/hotpath.rs"));

fn hotpath_bench_function() -> &'static str {
    const PREFIX: &str = "const BENCH_FUNCTION: &str = \"";

    let declarations = HOTPATH_SOURCE
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(PREFIX)
                .and_then(|value| value.strip_suffix("\";"))
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 1, "hotpath must declare one bench name");
    declarations[0]
}

fn bench_scripts(bench_dir: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(bench_dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", bench_dir.display()));
    let mut scripts = entries
        .map(|entry| entry.unwrap_or_else(|error| panic!("cannot read bench entry: {error}")))
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == OsStr::new("lua"))
        })
        .collect::<Vec<_>>();
    scripts.sort();
    scripts
}

#[derive(Debug, PartialEq, Eq)]
struct CostFingerprint {
    own_cost: u64,
    arithmetic_ops: u64,
    negations: u64,
    table_creations: u64,
    table_writes: u64,
    array_elements: u64,
    function_calls: u64,
    instructions: u64,
    nested: Vec<CostFingerprint>,
}

fn cost_fingerprint(scope: &ScopeCost) -> CostFingerprint {
    CostFingerprint {
        own_cost: scope.own_cost,
        arithmetic_ops: scope.arithmetic_ops(),
        negations: scope.negations(),
        table_creations: scope.table_creations(),
        table_writes: scope.table_writes(),
        array_elements: scope.array_elements(),
        function_calls: scope.function_calls(),
        instructions: scope.instructions,
        nested: scope.nested.iter().map(cost_fingerprint).collect(),
    }
}

#[derive(Debug)]
struct WorkloadPair {
    name: String,
    file: String,
    xxh128: String,
    hotpath_file: String,
    hotpath_xxh128: String,
}

fn workload_pairs() -> Vec<WorkloadPair> {
    let config = support::brokkr_config();
    let workloads = config
        .get("dellingr")
        .and_then(|dellingr| dellingr.get("workloads"))
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| panic!("brokkr.toml has no [dellingr.workloads] table"));

    assert!(
        !workloads.is_empty(),
        "brokkr.toml contains no workload registrations"
    );
    workloads
        .iter()
        .map(|(name, registration)| {
            assert!(
                registration.is_table(),
                "workload {name} registration in brokkr.toml must be a table"
            );
            WorkloadPair {
                name: name.clone(),
                file: workload_string(registration, "file", name),
                xxh128: workload_string(registration, "xxh128", name),
                hotpath_file: workload_string(registration, "hotpath_file", name),
                hotpath_xxh128: workload_string(registration, "hotpath_xxh128", name),
            }
        })
        .collect()
}

fn workload_string(registration: &toml::Value, key: &str, workload: &str) -> String {
    let value = registration
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("workload {workload} {key} must be a string in brokkr.toml"));
    assert!(
        !value.is_empty(),
        "workload {workload} {key} must not be empty in brokkr.toml"
    );
    value.to_owned()
}

fn runtime_source(
    source: &str,
    expected_loop: &str,
    expected_print: &str,
) -> Result<String, String> {
    let lines = source.lines().collect::<Vec<_>>();
    let footer_lines = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if footer_lines.len() < 2 {
        return Err("standalone footer is missing".to_owned());
    }

    let loop_index = footer_lines[footer_lines.len() - 2];
    let print_index = footer_lines[footer_lines.len() - 1];
    if lines[loop_index].trim() != expected_loop || lines[print_index].trim() != expected_print {
        return Err("standalone footer changed before runtime validation".to_owned());
    }

    Ok(lines
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != loop_index && *index != print_index)
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn one_kernel_source(source: &str) -> Result<(String, bool), String> {
    let mut lines = Vec::new();
    let mut patched_repeat = false;

    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(after_name) = trimmed.strip_prefix("local K") else {
            lines.push(line.to_owned());
            continue;
        };
        let Some(repeats) = after_name.trim_start().strip_prefix('=') else {
            lines.push(line.to_owned());
            continue;
        };
        if patched_repeat {
            return Err("more than one local K declaration".to_owned());
        }
        let repeats = repeats.trim_start();
        let digits = repeats.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err("local K must have an integer value".to_owned());
        }
        let suffix = &repeats[digits..];
        if !suffix.trim().is_empty() && !suffix.trim_start().starts_with("--") {
            return Err("local K must use a single integer literal".to_owned());
        }

        let indentation = &line[..line.len() - trimmed.len()];
        lines.push(format!("{indentation}local K = 1{suffix}"));
        patched_repeat = true;
    }

    Ok((lines.join("\n"), patched_repeat))
}

fn run_bench_once(script_path: &Path, source: &str, bench_function: &str) -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative_path = script_path.strip_prefix(root).map_err(|error| {
        format!(
            "{} is outside the repository: {error}",
            script_path.display()
        )
    })?;
    let display_path = relative_path.to_string_lossy().replace('\\', "/");
    let display_stem = relative_path
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");
    let expected_loop = format!("for i = 1, 30 do {bench_function}() end");
    let expected_print = format!("print(\"{display_stem}: true\")");
    let source = runtime_source(source, &expected_loop, &expected_print)
        .map_err(|error| format!("{display_path}: {error}"))?;
    let (mut source, patched_repeat) =
        one_kernel_source(&source).map_err(|error| format!("{display_path}: {error}"))?;
    let local_kernel = source
        .lines()
        .any(|line| line.trim() == "local function kernel()");
    let kernel_probe = "__dellingr_bench_kernel_probe";
    let function_to_call = if !patched_repeat && local_kernel {
        source.push_str(&format!(
            "\nfunction {kernel_probe}() return kernel() end\n"
        ));
        kernel_probe
    } else {
        bench_function
    };
    let program = Program::compile(&source, Some(display_path.as_bytes()))
        .map_err(|error| format!("{display_path} does not parse: {error}"))?;

    let mut state = State::new();
    state.set_cost_budget(BENCH_SETUP_COST_BUDGET);
    state
        .load(&program)
        .map_err(|error| format!("{display_path} does not load: {error}"))?;
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .map_err(|error| format!("{display_path} setup failed: {error}"))?;
    state
        .get_global(bench_function)
        .map_err(|error| format!("{display_path} cannot read {bench_function}: {error}"))?;
    let actual_type = state.typ(-1);
    if actual_type != LuaType::Function {
        return Err(format!(
            "{display_path} did not install a global {bench_function} function (got {actual_type})"
        ));
    }
    state
        .pop(1)
        .map_err(|error| format!("{display_path} cannot clear the checked function: {error}"))?;
    state
        .get_global(function_to_call)
        .map_err(|error| format!("{display_path} cannot read {function_to_call}: {error}"))?;
    if state.typ(-1) != LuaType::Function {
        return Err(format!(
            "{display_path} did not install a callable {function_to_call}"
        ));
    }

    state.set_cost_budget(BENCH_KERNEL_COST_BUDGET);
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .map_err(|error| format!("{display_path} {function_to_call} failed: {error}"))
}

fn run_bench_scripts(scripts: &[(PathBuf, String)], bench_function: &str) -> Vec<String> {
    support::parallel_failures(scripts, BENCH_SCRIPT_WORKERS, |(path, source)| {
        run_bench_once(path, source, bench_function).err()
    })
}

fn scope<'a>(analysis: &'a dellingr::CostAnalysis, name: &str) -> &'a ScopeCost {
    analysis
        .root
        .nested
        .iter()
        .find(|scope| scope.name == name)
        .unwrap_or_else(|| panic!("cost analysis has no top-level function {name}"))
}

fn shared_helper_fingerprints(
    analysis: &dellingr::CostAnalysis,
    excluded_names: &[&str],
) -> std::collections::BTreeMap<String, CostFingerprint> {
    analysis
        .root
        .nested
        .iter()
        .filter(|scope| !excluded_names.contains(&scope.name.as_str()))
        .map(|scope| (scope.name.clone(), cost_fingerprint(scope)))
        .collect()
}

#[test]
fn registered_workload_file_digests_match_brokkr_pins() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pairs = workload_pairs();

    for pair in pairs {
        let workload_name = pair.name.as_str();
        for (digest_key, file, expected) in [
            ("xxh128", pair.file.as_str(), pair.xxh128.as_str()),
            (
                "hotpath_xxh128",
                pair.hotpath_file.as_str(),
                pair.hotpath_xxh128.as_str(),
            ),
        ] {
            let path = root.join(file);
            let bytes = fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "cannot read workload registration [dellingr.workloads.{workload_name}] pinned file {file}: {error}"
                )
            });
            let actual = format!("{:032x}", xxh3_128(&bytes));
            assert_eq!(
                actual, expected,
                "workload registration [dellingr.workloads.{workload_name}].{digest_key} drifted for file {file}: expected {expected}, actual {actual}. Editing a pinned file means re-registering its pin, which cuts its stored benchmark history."
            );
        }
    }
}

#[test]
fn registered_workload_files_cover_bench_scripts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pairs = workload_pairs();
    let bench_dir = root.join("bench");
    let mut expected_files = bench_scripts(&bench_dir)
        .iter()
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or_else(|error| {
                    panic!("{} is outside the repository: {error}", path.display())
                })
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<std::collections::BTreeSet<_>>();

    for &(workload, file) in SAME_FILE_WORKLOAD_ALLOWLIST {
        assert!(
            expected_files.insert(file.to_owned()),
            "workload {workload} exception overlaps a bench file: {file}"
        );
        let pair = pairs
            .iter()
            .find(|pair| pair.name == workload)
            .unwrap_or_else(|| panic!("workload {workload} is missing from brokkr.toml"));
        assert_eq!(
            pair.file, file,
            "workload {workload} must use its listed non-bench file"
        );
    }

    let registered_files = pairs
        .iter()
        .map(|pair| pair.file.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        registered_files, expected_files,
        "registered workload file values must cover every bench Lua file, with only listed non-bench files"
    );
}

#[test]
fn bench_scripts_parse_and_follow_the_standalone_footer_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bench_dir = root.join("bench");
    let scripts = bench_scripts(&bench_dir);
    assert!(!scripts.is_empty(), "bench/ contains no Lua scripts");

    let bench_function = hotpath_bench_function();
    let expected_loop = format!("for i = 1, 30 do {bench_function}() end");
    let mut scripts_to_execute = Vec::with_capacity(scripts.len());

    for script_path in scripts {
        let relative_path = script_path.strip_prefix(&root).unwrap_or_else(|error| {
            panic!(
                "{} is outside the repository: {error}",
                script_path.display()
            )
        });
        let source = fs::read_to_string(&script_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", script_path.display()));

        let expected_declaration = format!("function {bench_function}()");
        assert_eq!(
            source
                .lines()
                .map(str::trim)
                .filter(|line| *line == expected_declaration.as_str())
                .count(),
            1,
            "{} must define {bench_function} exactly once",
            script_path.display()
        );

        // The success line names the script without its `.lua` extension.
        let display_stem = relative_path
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let expected_print = format!("print(\"{display_stem}: true\")");
        let success_lines = source
            .lines()
            .map(str::trim)
            .filter(|line| line.contains(": true"))
            .collect::<Vec<_>>();
        assert_eq!(
            success_lines.len(),
            1,
            "{} must contain exactly one success line",
            script_path.display()
        );
        assert_eq!(
            success_lines[0],
            expected_print.as_str(),
            "{}",
            script_path.display()
        );

        let nonempty_lines = source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert!(
            nonempty_lines.len() >= 2,
            "{} is missing its standalone footer",
            script_path.display()
        );
        assert_eq!(
            nonempty_lines[nonempty_lines.len() - 2],
            expected_loop.as_str(),
            "{} must run its bench 30 times at the end",
            script_path.display()
        );
        assert_eq!(
            nonempty_lines[nonempty_lines.len() - 1],
            expected_print.as_str(),
            "{} must print its success line last",
            script_path.display()
        );

        scripts_to_execute.push((script_path, source));
    }

    let failures = run_bench_scripts(&scripts_to_execute, bench_function);
    assert!(
        failures.is_empty(),
        "bench scripts failed runtime validation:\n{}",
        failures.join("\n")
    );
}

#[test]
fn registered_workload_pairs_keep_kernel_cost_fingerprints_aligned() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pairs = workload_pairs();

    for pair in pairs {
        if pair.file == pair.hotpath_file {
            assert!(
                SAME_FILE_WORKLOAD_ALLOWLIST.iter().any(|(name, file)| {
                    *name == pair.name.as_str() && *file == pair.file.as_str()
                }),
                "workload {} uses the same file for file and hotpath_file without an explicit exception: {}",
                pair.name,
                pair.file
            );
            continue;
        }

        let bench_path = root.join(&pair.file);
        let hotpath_path = root.join(&pair.hotpath_file);
        assert!(
            bench_path.is_file(),
            "workload {} file does not exist: {}",
            pair.name,
            pair.file
        );
        assert!(
            hotpath_path.is_file(),
            "workload {} hotpath_file does not exist: {}",
            pair.name,
            pair.hotpath_file
        );

        let bench_source = fs::read_to_string(&bench_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", bench_path.display()));
        let hotpath_source = fs::read_to_string(&hotpath_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", hotpath_path.display()));
        let bench_analysis = Program::compile(&bench_source, None)
            .unwrap_or_else(|error| panic!("{} does not parse: {error}", bench_path.display()))
            .analyze_cost();
        let hotpath_analysis = Program::compile(&hotpath_source, None)
            .unwrap_or_else(|error| panic!("{} does not parse: {error}", hotpath_path.display()))
            .analyze_cost();

        // Verdict files usually put the kernel in `kernel()` and call it from
        // `_bench()` K times. Hotpath examples put that same body directly in
        // `_bench()`. The composite and parse probes use `_bench()` on both
        // sides, so prefer the helper only when it exists.
        let bench_kernel_name = if bench_analysis
            .root
            .nested
            .iter()
            .any(|scope| scope.name == "kernel")
        {
            "kernel"
        } else {
            "_bench"
        };
        let bench_kernel = scope(&bench_analysis, bench_kernel_name);
        let hotpath_kernel = scope(&hotpath_analysis, "_bench");
        assert_eq!(
            cost_fingerprint(bench_kernel),
            cost_fingerprint(hotpath_kernel),
            "workload {} kernel changed between {} and {}",
            pair.name,
            pair.file,
            pair.hotpath_file
        );

        assert_eq!(
            shared_helper_fingerprints(&bench_analysis, &["_bench", "kernel"]),
            shared_helper_fingerprints(&hotpath_analysis, &["_bench", "kernel"]),
            "workload {} helper functions changed between {} and {}",
            pair.name,
            pair.file,
            pair.hotpath_file
        );
    }
}
