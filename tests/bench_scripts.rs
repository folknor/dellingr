use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use dellingr::{ScopeCost, State, analyze_cost};

// Keep the expected function name tied to the constant used by the harness.
const HOTPATH_SOURCE: &str = include_str!("../examples/hotpath.rs");

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
        arithmetic_ops: scope.arithmetic_ops,
        negations: scope.negations,
        table_creations: scope.table_creations,
        table_writes: scope.table_writes,
        array_elements: scope.array_elements,
        function_calls: scope.function_calls,
        instructions: scope.instructions,
        nested: scope.nested.iter().map(cost_fingerprint).collect(),
    }
}

#[derive(Debug)]
struct WorkloadPair {
    name: String,
    file: String,
    hotpath_file: String,
}

fn workload_pairs(root: &Path) -> Vec<WorkloadPair> {
    let source =
        fs::read_to_string(root.join("brokkr.toml")).expect("brokkr.toml should be readable");
    let mut pairs = Vec::new();
    let mut current: Option<WorkloadPair> = None;

    for line in source.lines().chain(std::iter::once("[end]")) {
        let line = line.trim();
        if line.starts_with('[') {
            if let Some(pair) = current.take() {
                pairs.push(pair);
            }

            if let Some(name) = line
                .strip_prefix("[dellingr.workloads.")
                .and_then(|name| name.strip_suffix(']'))
            {
                assert!(!name.is_empty(), "workload name in brokkr.toml is empty");
                current = Some(WorkloadPair {
                    name: name.to_owned(),
                    file: String::new(),
                    hotpath_file: String::new(),
                });
            }
            continue;
        }

        let Some(pair) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "file" => pair.file = parse_toml_string(value, "file", &pair.name),
            "hotpath_file" => {
                pair.hotpath_file = parse_toml_string(value, "hotpath_file", &pair.name);
            }
            _ => {}
        }
    }

    assert!(
        !pairs.is_empty(),
        "brokkr.toml contains no workload registrations"
    );
    for pair in &pairs {
        assert!(!pair.file.is_empty(), "workload {} has no file", pair.name);
        assert!(
            !pair.hotpath_file.is_empty(),
            "workload {} has no hotpath_file",
            pair.name
        );
    }
    pairs
}

fn parse_toml_string(value: &str, key: &str, workload: &str) -> String {
    let value = value.trim();
    let Some(quoted) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        panic!("workload {workload} {key} should be a quoted TOML string");
    };
    quoted.to_owned()
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
fn bench_scripts_parse_and_follow_the_standalone_footer_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bench_dir = root.join("bench");
    let scripts = bench_scripts(&bench_dir);
    assert!(!scripts.is_empty(), "bench/ contains no Lua scripts");

    let bench_function = hotpath_bench_function();
    let expected_loop = format!("for i = 1, 30 do {bench_function}() end");

    for script_path in scripts {
        let relative_path = script_path.strip_prefix(&root).unwrap_or_else(|error| {
            panic!(
                "{} is outside the repository: {error}",
                script_path.display()
            )
        });
        let display_path = relative_path.to_string_lossy().replace('\\', "/");
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

        let mut state = State::new();
        if let Err(error) = state.load_string_named(&source, Some(format!("@{display_path}"))) {
            panic!("{} does not parse: {error}", script_path.display());
        }
    }
}

#[test]
fn registered_workload_pairs_keep_kernel_cost_fingerprints_aligned() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pairs = workload_pairs(&root);

    for pair in pairs {
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
        let bench_analysis = analyze_cost(&bench_source)
            .unwrap_or_else(|error| panic!("{} does not parse: {error}", bench_path.display()));
        let hotpath_analysis = analyze_cost(&hotpath_source)
            .unwrap_or_else(|error| panic!("{} does not parse: {error}", hotpath_path.display()));

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
