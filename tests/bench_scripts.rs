use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use dellingr::State;

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
