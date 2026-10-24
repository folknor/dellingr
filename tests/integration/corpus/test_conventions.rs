//! Conventions for the test tree itself, checked lexically over its sources.
//! Each check is an inventory of the constructs it names, not a proof about
//! every way Rust can spell the same thing.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("test source directory should be readable") {
        let path = entry.expect("test source entry should be readable").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()))
}

/// Every Rust source under `tests/`, with its path relative to `tests/`.
fn test_sources() -> Vec<(PathBuf, String)> {
    let tests_dir = tests_dir();
    let mut paths = Vec::new();
    collect_rust_files(&tests_dir, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let source = read(&path);
            let relative = path
                .strip_prefix(&tests_dir)
                .expect("test source should be under tests/")
                .to_path_buf();
            (relative, source)
        })
        .collect()
}

/// `source` with each comment (nested block comments included) replaced by a
/// space, as Rust's tokenizer treats it, and string, raw string and char
/// literals emptied, keeping every newline, so the checks below see only code
/// and line numbers still match.
fn code_only(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(character) = rest.chars().next() {
        let (consumed, kept) = if rest.starts_with("//") {
            (rest.find('\n').unwrap_or(rest.len()), " ".to_owned())
        } else if rest.starts_with("/*") {
            let length = block_comment_length(rest);
            (length, format!(" {}", newlines(&rest[..length])))
        } else if let Some(length) = raw_string_length(rest) {
            (length, format!("\"\"{}", newlines(&rest[..length])))
        } else if let Some(length) = char_literal_length(rest) {
            (length, "' '".to_owned())
        } else if character == '"' {
            let length = string_length(rest);
            (length, format!("\"\"{}", newlines(&rest[..length])))
        } else {
            (character.len_utf8(), character.to_string())
        };
        out.push_str(&kept);
        rest = &rest[consumed..];
    }
    out
}

fn newlines(text: &str) -> String {
    text.chars()
        .filter(|&character| character == '\n')
        .collect()
}

fn block_comment_length(rest: &str) -> usize {
    let mut depth = 0usize;
    let mut index = 0;
    while index < rest.len() {
        if rest[index..].starts_with("/*") {
            depth += 1;
            index += 2;
        } else if rest[index..].starts_with("*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += rest[index..].chars().next().map_or(1, char::len_utf8);
        }
    }
    rest.len()
}

/// The length of a `"..."` literal starting at `rest`, escapes included.
fn string_length(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    rest.len()
}

/// The length of a raw string literal (`r"..."`, `r#"..."#`) starting at
/// `rest`, if one does. A preceding `b` or `c` has already been copied.
fn raw_string_length(rest: &str) -> Option<usize> {
    let after_r = rest.strip_prefix('r')?;
    let hashes = after_r.bytes().take_while(|&byte| byte == b'#').count();
    let body = after_r[hashes..].strip_prefix('"')?;
    let close = format!("\"{}", "#".repeat(hashes));
    let end = body.find(&close)?;
    Some(1 + hashes + 1 + end + close.len())
}

/// The length of a char literal (`'x'`, `'\n'`, `'\u{2014}'`) starting at
/// `rest`, if one does; a lifetime is not one.
fn char_literal_length(rest: &str) -> Option<usize> {
    let body = rest.strip_prefix('\'')?;
    if let Some(escaped) = body.strip_prefix('\\') {
        let close = escaped.find('\'')?;
        return (close <= 10).then_some(close + 3);
    }
    let character = body.chars().next()?;
    body[character.len_utf8()..]
        .starts_with('\'')
        .then_some(character.len_utf8() + 2)
}

/// One logical line of `code_only` output: an attribute that rustfmt wrapped
/// over several lines is joined onto its first.
struct LogicalLine {
    /// Zero-based index of the first physical line.
    index: usize,
    /// Zero-based index of the last physical line.
    end: usize,
    text: String,
    /// Open `(`, `[` and `{` before the line starts.
    depth: usize,
}

fn logical_lines(code: &str) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut depth = 0usize;
    let mut pending: Option<LogicalLine> = None;
    for (index, line) in code.lines().enumerate() {
        let trimmed = line.trim();
        let start_depth = depth;
        for character in trimmed.chars() {
            match character {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if let Some(mut open) = pending.take() {
            open.text.push(' ');
            open.text.push_str(trimmed);
            open.end = index;
            if depth > open.depth {
                pending = Some(open);
            } else {
                lines.push(open);
            }
        } else {
            let line = LogicalLine {
                index,
                end: index,
                text: trimmed.to_owned(),
                depth: start_depth,
            };
            if trimmed.starts_with('#') && depth > start_depth {
                pending = Some(line);
            } else {
                lines.push(line);
            }
        }
    }
    lines.extend(pending);
    lines
}

/// Identifier-like tokens of a line of code.
fn tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
}

// Test filesystem writes and CLI paths are checked before compilation by
// brokkr's test-scoped textlint rules, including unit tests under src/.

/// Cargo makes every `tests/*.rs` and `tests/*/main.rs` its own test binary.
/// The suites share the integration crate. `native_stack` isolates stack
/// overflow probes; `libm_symbols` lets release sweeps inspect their own CLI
/// without compiling the full integration suite. These are named exceptions.
#[test]
fn the_test_tree_holds_only_the_named_test_targets() {
    let entries: BTreeSet<String> = fs::read_dir(tests_dir())
        .expect("tests/ should be readable")
        .map(|entry| {
            entry
                .expect("tests/ entry should be readable")
                .file_name()
                .into_string()
                .expect("tests/ entry names are UTF-8")
        })
        .collect();
    let expected: BTreeSet<String> = [
        "common",
        "fixtures",
        "integration",
        "libm_symbols.rs",
        "native_stack.rs",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        entries, expected,
        "a new suite is a module of tests/integration, not a new test target"
    );
    for directory in ["common", "fixtures"] {
        assert!(
            !tests_dir().join(directory).join("main.rs").exists(),
            "tests/{directory} holds included files, not a test target"
        );
    }
}

/// `mod` declarations allowed to carry a `cfg`: (declaring file relative to
/// `tests/integration`, module name, the whole attribute).
const GATED_MODULES: &[(&str, &str, &str)] = &[
    ("main.rs", "snapshot", "#[cfg(feature = \"snapshot\")]"),
    (
        "support/mod.rs",
        "save_golden_program",
        "#[cfg(feature = \"snapshot\")]",
    ),
];

/// A source file under `tests/integration` that the module tree does not
/// reach is never compiled, so its tests silently never run. Walks the
/// `mod NAME;` declarations from `main.rs` and requires every file to be
/// reached, with a `cfg` only where `GATED_MODULES` allows one.
#[test]
fn every_integration_source_is_reached_from_main() {
    let root =
        fs::canonicalize(tests_dir().join("integration")).expect("tests/integration should exist");
    let mut reached = BTreeSet::new();
    let mut pending = vec![root.join("main.rs")];
    while let Some(file) = pending.pop() {
        let file = fs::canonicalize(&file)
            .unwrap_or_else(|error| panic!("{} should exist: {error}", file.display()));
        if reached.insert(file.clone()) {
            pending.extend(declared_modules(&root, &file));
        }
    }
    let mut sources = Vec::new();
    collect_rust_files(&root, &mut sources);
    for source in sources {
        let source = fs::canonicalize(&source).expect("a listed source exists");
        assert!(
            reached.contains(&source),
            "{} is not reached by mod declarations from tests/integration/main.rs, so it never compiles",
            source.display()
        );
    }
}

/// The files `file`'s `mod NAME;` declarations name. Only a declaration that
/// is a whole line at the file's top level is resolved; one anywhere else (in
/// an inline module, inside macro input, after an attribute on the same line)
/// fails as a form this walk does not model.
fn declared_modules(root: &Path, file: &Path) -> Vec<PathBuf> {
    let source = read(file);
    let raw_lines: Vec<&str> = source.lines().collect();
    // A `#[path]` target outside the crate directory keeps its full path,
    // which no GATED_MODULES entry names.
    let relative = file
        .strip_prefix(root)
        .map_or_else(|_| file.to_path_buf(), Path::to_path_buf);
    let directory = file.parent().expect("a source file has a directory");
    let stem = file.file_stem().expect("a source file has a stem");
    // rustc: the children of the crate root and of a `mod.rs` sit beside
    // them; the children of any other `x.rs`, a nested `main.rs` included,
    // sit in `x/`.
    let child_directory = if relative == Path::new("main.rs") || stem == "mod" {
        directory.to_path_buf()
    } else {
        directory.join(stem)
    };
    let mut children = Vec::new();
    // The pending item's attributes, as written (literals intact).
    let mut attributes: Vec<String> = Vec::new();
    for line in logical_lines(&code_only(&source)) {
        let text = line.text.as_str();
        let declaration = text
            .strip_prefix("pub(crate) ")
            .unwrap_or(text)
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
            .filter(|name| tokens(name).count() == 1 && !name.contains(' '));
        let mentions_mod = {
            let words: Vec<&str> = tokens(text).collect();
            words.windows(2).any(|pair| pair[0] == "mod") && text.contains(';')
        };
        match declaration {
            Some(name) if line.depth == 0 => {
                let mut path_attribute = None;
                for attribute in &attributes {
                    if attribute.starts_with("#[path") {
                        path_attribute = Some(raw_path(attribute, file));
                        continue;
                    }
                    let allowed = GATED_MODULES.iter().any(|(declaring, module, gate)| {
                        relative == Path::new(declaring) && name == *module && attribute == gate
                    });
                    assert!(
                        allowed,
                        "{}: `mod {name};` carries `{attribute}`, which GATED_MODULES does not allow",
                        file.display()
                    );
                }
                children.push(match path_attribute {
                    Some(path) => directory.join(path),
                    None => {
                        let flat = child_directory.join(format!("{name}.rs"));
                        if flat.is_file() {
                            flat
                        } else {
                            child_directory.join(name).join("mod.rs")
                        }
                    }
                });
            }
            _ => assert!(
                !mentions_mod,
                "{}:{}: a `mod` declaration this walk does not model; declare file modules on their own line at the top level",
                file.display(),
                line.index + 1
            ),
        }
        if text.starts_with("#[") {
            let written: Vec<&str> = raw_lines[line.index..=line.end]
                .iter()
                .map(|raw| raw.trim())
                .collect();
            attributes.push(written.join(" "));
        } else if !text.is_empty() {
            attributes.clear();
        }
    }
    children
}

/// A libtest filter is a substring of the test path, so `brokkr test
/// failures::` also selects any `x_failures::` or nested `failures::`
/// module. No module name under `tests/integration` (file, directory or
/// inline) may equal or end with a subject's name, a module `main.rs`
/// declares at its top level, except that declaration itself. Names are taken
/// from the `mod` declarations (a `#[path]` module is named by its
/// declaration, not its file), in every file under `tests/integration` and
/// `tests/common`. Library unit tests share the filter too (`compiler::` also
/// runs the library's compiler tests); that overlap is by subject, and this
/// check does not police `src/`.
#[test]
fn subject_filters_select_only_their_subject() {
    let root = tests_dir().join("integration");
    let main = root.join("main.rs");
    let subjects: BTreeSet<String> = module_declarations(&code_only(&read(&main)))
        .into_iter()
        .filter_map(|(name, depth)| (depth == 0).then_some(name))
        .collect();
    let mut sources = Vec::new();
    collect_rust_files(&root, &mut sources);
    collect_rust_files(&tests_dir().join("common"), &mut sources);
    for source in sources {
        for (name, depth) in module_declarations(&code_only(&read(&source))) {
            if source == main && depth == 0 {
                // A subject's own declaration.
                continue;
            }
            for subject in &subjects {
                assert!(
                    !name.ends_with(subject.as_str()),
                    "{}: module `{name}` ends with subject `{subject}`, so `{subject}::` would select it",
                    source.display()
                );
            }
        }
    }
    for subject in &subjects {
        for other in &subjects {
            assert!(
                subject == other || !subject.ends_with(other.as_str()),
                "subject `{subject}` ends with subject `{other}`"
            );
        }
    }
}

/// Every module `code` (from `code_only`) declares, file or inline, with the
/// number of `(`, `[` and `{` open where it is declared: each `mod` keyword
/// followed, across any whitespace, by a name and then `;` or `{`.
fn module_declarations(code: &str) -> Vec<(String, usize)> {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut declarations = Vec::new();
    let mut depth = 0usize;
    let mut scanned = 0;
    for (at, keyword) in code.match_indices("mod") {
        for character in code[scanned..at].chars() {
            match character {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        scanned = at;
        let before = code[..at].chars().next_back();
        let rest = &code[at + keyword.len()..];
        if before.is_some_and(is_word) || !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim_start();
        let name_length = rest
            .find(|character: char| !is_word(character))
            .unwrap_or(rest.len());
        let (name, after) = rest.split_at(name_length);
        if !name.is_empty() && after.trim_start().starts_with([';', '{']) {
            declarations.push((name.to_owned(), depth));
        }
    }
    declarations
}

/// The path in a `#[path = "..."]` attribute of `file`.
fn raw_path(attribute: &str, file: &Path) -> String {
    attribute
        .strip_prefix("#[path = \"")
        .and_then(|rest| rest.strip_suffix("\"]"))
        .unwrap_or_else(|| {
            panic!(
                "{}: write a path attribute as `#[path = \"...\"]` on one line",
                file.display()
            )
        })
        .to_owned()
}

/// A lint allowance that hides dead code, and an inner `cfg` that gates a
/// whole file (the gated `mod` line is the audited form), are both
/// forbidden: in one crate, a helper every module stopped using is reported.
#[test]
fn no_dead_code_allowances_or_file_level_gates() {
    for (relative_path, source) in test_sources() {
        for line in logical_lines(&code_only(&source)) {
            let text = line.text.as_str();
            let location = format!("tests/{}:{}", relative_path.display(), line.index + 1);
            if text.starts_with('#') {
                let words: Vec<&str> = tokens(text).collect();
                let lint_list = words.iter().any(|word| matches!(*word, "allow" | "expect"));
                let names_dead = words
                    .iter()
                    .any(|word| matches!(*word, "dead_code" | "unused"));
                assert!(
                    !(lint_list && names_dead),
                    "{location}: dead-code allowance; delete what is unused instead"
                );
            }
            assert!(
                !text.starts_with("#![cfg"),
                "{location}: a file-level cfg; gate the mod line and list it in GATED_MODULES"
            );
        }
    }
}

/// The integration suites share one process, and outside brokkr its tests
/// run on parallel threads. Each construct below can carry state from one
/// test to another, so each use needs a `// process-global:` comment on its
/// line or the line above, saying why no other test can observe it: every
/// `static` item, and the tokens `thread_local`, `set_hook`, `take_hook`,
/// `set_var`, `remove_var` and `set_current_dir` (as tokens, so an aliasing
/// `use` is caught too).
#[test]
fn process_global_constructs_are_marked() {
    let flagged_tokens = [
        concat!("thread", "_local"),
        concat!("set_", "hook"),
        concat!("take_", "hook"),
        concat!("set_", "var"),
        concat!("remove_", "var"),
        concat!("set_current", "_dir"),
    ];
    let justification = concat!("// process", "-global:");
    for (relative_path, source) in test_sources() {
        let raw_lines: Vec<&str> = source.lines().collect();
        for (index, line) in code_only(&source).lines().enumerate() {
            let flagged =
                has_static_keyword(line) || tokens(line).any(|word| flagged_tokens.contains(&word));
            if !flagged {
                continue;
            }
            let marked = raw_lines[index].contains(justification)
                || (index > 0 && raw_lines[index - 1].contains(justification));
            assert!(
                marked,
                "tests/{}:{}: process-wide state needs a `{justification}` comment",
                relative_path.display(),
                index + 1
            );
        }
    }
}

/// Curated check shapes filter tests by substring. Each filter must select an
/// existing test, and feature gates on matched tests identify the families
/// that the shape must keep selected.
#[test]
fn curated_check_filters_select_existing_feature_gated_tests() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let document = crate::support::brokkr_config();
    let checks = document
        .get("check")
        .and_then(toml::Value::as_array)
        .expect("brokkr.toml should list check shapes");

    let mut sources = Vec::new();
    collect_rust_files(&manifest_dir.join("src"), &mut sources);
    collect_rust_files(&manifest_dir.join("tests/integration"), &mut sources);
    let mut gated_modules = Vec::new();
    for source_path in &sources {
        let source = read(source_path);
        let base = test_module_path(&manifest_dir, source_path);
        let is_library_source = source_path.starts_with(manifest_dir.join("src"));
        let (modules, _) = feature_gated_items(&source, &base, &BTreeSet::new(), is_library_source);
        gated_modules.extend(
            modules
                .into_iter()
                .map(|(module, features)| (is_library_source, module, features)),
        );
    }

    let mut tests = Vec::new();
    for source_path in &sources {
        let source = read(source_path);
        let base = test_module_path(&manifest_dir, source_path);
        let is_library_source = source_path.starts_with(manifest_dir.join("src"));
        // The library and the integration crate share module names
        // (`compiler`, say), so a gate only applies within its own crate.
        let inherited_features = gated_modules
            .iter()
            .filter(|(library, module, _)| {
                *library == is_library_source && base.starts_with(module)
            })
            .flat_map(|(_, _, features)| features.iter().cloned())
            .collect();
        let (_, source_tests) =
            feature_gated_items(&source, &base, &inherited_features, is_library_source);
        tests.extend(source_tests);
    }

    // The native-stack target is a separate crate with its own curated
    // observation lane. Keep its explicit tests in the filter inventory too.
    let (_, native_tests) = feature_gated_items(
        &read(&manifest_dir.join("tests/native_stack.rs")),
        &[],
        &BTreeSet::new(),
        false,
    );

    for check in checks
        .iter()
        .filter(|check| check.get("curated").and_then(toml::Value::as_bool) == Some(true))
    {
        let name = check
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("curated check shapes should have names");
        assert!(
            check.get("profile").is_none()
                && document
                    .get("test")
                    .and_then(|settings| settings.get("debug"))
                    .and_then(toml::Value::as_bool)
                    == Some(true),
            "curated check shape `{name}` must use the default dev profile"
        );
        let filters = check
            .get("only")
            .and_then(toml::Value::as_array)
            .unwrap_or_else(|| panic!("curated check shape `{name}` should list its only filters"))
            .iter()
            .map(|filter| {
                let filter = filter.as_str().unwrap_or_else(|| {
                    panic!("curated check shape `{name}` filters should be strings")
                });
                assert!(
                    !filter.is_empty(),
                    "curated check shape `{name}` should not have an empty only filter"
                );
                filter
            })
            .collect::<Vec<_>>();
        assert!(
            !filters.is_empty(),
            "curated check shape `{name}` should list at least one only filter"
        );

        let shape_features = check
            .get("features")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .map(|feature| {
                feature.as_str().unwrap_or_else(|| {
                    panic!("curated check shape `{name}` features should be strings")
                })
            })
            .collect::<BTreeSet<_>>();
        let includes_library = check.get("lib_tests").and_then(toml::Value::as_bool) == Some(true);
        let includes_integration = check
            .get("tests")
            .and_then(toml::Value::as_array)
            .is_some_and(|targets| {
                targets
                    .iter()
                    .any(|target| target.as_str() == Some("integration"))
            });
        let includes_native_stack = check
            .get("tests")
            .and_then(toml::Value::as_array)
            .is_some_and(|targets| {
                targets
                    .iter()
                    .any(|target| target.as_str() == Some("native_stack"))
            });
        let shape_tests = tests
            .iter()
            .filter(|test| {
                if test.is_library {
                    includes_library
                } else {
                    includes_integration
                }
            })
            .chain(native_tests.iter().filter(|_| includes_native_stack))
            .collect::<Vec<_>>();
        for filter in &filters {
            assert!(
                shape_tests.iter().any(|test| test.path.contains(filter)),
                "curated check shape `{name}` only filter `{filter}` does not match a test in its selected targets"
            );
        }

        let selected_features = shape_tests
            .iter()
            .filter(|test| filters.iter().any(|filter| test.path.contains(filter)))
            .flat_map(|test| test.features.iter())
            .filter(|feature| shape_features.contains(feature.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>();

        for test in shape_tests.iter().filter(|test| {
            test.features
                .iter()
                .any(|feature| selected_features.contains(feature))
        }) {
            assert!(
                filters.iter().any(|filter| test.path.contains(filter)),
                "feature-gated test `{}` is not selected by curated check shape `{name}` `only` filters",
                test.path
            );
        }
    }
}

/// The Rust module path represented by a source file in `src/` or
/// `tests/integration/`.
fn test_module_path(manifest_dir: &Path, source_path: &Path) -> Vec<String> {
    let relative = source_path
        .strip_prefix(manifest_dir)
        .expect("a source path should be under the manifest directory");
    let relative = if relative.starts_with("src") {
        relative
            .strip_prefix("src")
            .expect("a library source should be under src/")
    } else {
        relative
            .strip_prefix(Path::new("tests/integration"))
            .expect("an integration source should be under tests/integration/")
    };

    let mut modules = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .map(|part| {
            part.as_os_str()
                .to_str()
                .expect("Rust source paths should be UTF-8")
                .to_owned()
        })
        .collect::<Vec<_>>();
    let file_name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a Rust source path should have a UTF-8 file name");
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("a Rust source file should have a UTF-8 stem");
    if !matches!(stem, "lib" | "main" | "mod") {
        modules.push(stem.to_owned());
    }
    modules
}

struct ModuleScope {
    name: String,
    body_depth: usize,
    features: BTreeSet<String>,
}

struct ScannedTest {
    path: String,
    features: BTreeSet<String>,
    is_library: bool,
}

/// Gated external modules (path and features) found in one source file.
type GatedModules = Vec<(Vec<String>, BTreeSet<String>)>;

/// Finds feature-gated external modules and tests in one Rust source file.
/// This follows module and function declarations after comments and literals
/// have been blanked, while reading the original attribute text for cfgs.
fn feature_gated_items(
    source: &str,
    base: &[String],
    inherited_features: &BTreeSet<String>,
    is_library_source: bool,
) -> (GatedModules, Vec<ScannedTest>) {
    let code = code_only(source);
    let raw_lines: Vec<&str> = source.lines().collect();
    let mut scopes: Vec<ModuleScope> = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut gated_modules = Vec::new();
    let mut tests = Vec::new();

    for line in logical_lines(&code) {
        while scopes
            .last()
            .is_some_and(|scope| scope.body_depth > line.depth)
        {
            scopes.pop();
        }
        let text = line.text.as_str();
        if text.is_empty() {
            continue;
        }
        if text.starts_with("#[") {
            attributes.push(raw_lines[line.index..=line.end].join(" "));
            continue;
        }

        let attribute_features = attributes
            .iter()
            .flat_map(|attribute| cfg_feature_names(attribute))
            .collect::<BTreeSet<_>>();
        let test_attribute = attributes
            .iter()
            .any(|attribute| attribute.contains("#[test]"));
        let mut current_features = inherited_features.clone();
        for scope in &scopes {
            current_features.extend(scope.features.iter().cloned());
        }
        current_features.extend(attribute_features);

        if let Some((name, inline)) = declared_module(text) {
            let mut module_path = base.to_vec();
            module_path.extend(scopes.iter().map(|scope| scope.name.clone()));
            module_path.push(name.clone());
            if inline {
                let net_braces = text
                    .chars()
                    .map(|character| match character {
                        '{' => 1isize,
                        '}' => -1,
                        _ => 0,
                    })
                    .sum::<isize>();
                if net_braces > 0 {
                    scopes.push(ModuleScope {
                        name,
                        body_depth: line.depth + 1,
                        features: current_features,
                    });
                }
            } else if !current_features.is_empty() {
                gated_modules.push((module_path, current_features));
            }
        } else if test_attribute && let Some(name) = declared_function(text) {
            let mut test_path = base.to_vec();
            test_path.extend(scopes.iter().map(|scope| scope.name.clone()));
            test_path.push(name);
            tests.push(ScannedTest {
                path: test_path.join("::"),
                features: current_features,
                is_library: is_library_source,
            });
        }

        attributes.clear();
    }

    (gated_modules, tests)
}

/// The features a `#[cfg(...)]` attribute requires. A `feature = "x"` inside
/// a `not(...)` excludes the item from that feature's builds rather than
/// gating it on them, so negated occurrences are skipped.
fn cfg_feature_names(attribute: &str) -> BTreeSet<String> {
    let mut features = BTreeSet::new();
    let trimmed = attribute.trim();
    let Some(predicate) = trimmed.strip_prefix("#[cfg(") else {
        return features;
    };

    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    // One entry per open parenthesis: whether that group is a `not(...)`.
    let mut groups: Vec<bool> = Vec::new();
    let mut rest = predicate;
    while let Some(character) = rest.chars().next() {
        if character == '(' {
            groups.push(false);
            rest = &rest[1..];
        } else if character == ')' {
            groups.pop();
            rest = &rest[1..];
        } else if character == '"' {
            // A string outside `feature = "..."`; skip to its close.
            let quoted = &rest[1..];
            rest = match quoted.find('"') {
                Some(close) => &quoted[close + 1..],
                None => "",
            };
        } else if is_word(character) {
            let length = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
            let word = &rest[..length];
            rest = &rest[length..];
            let after = rest.trim_start();
            if word == "not" && after.starts_with('(') {
                groups.push(true);
                rest = &after[1..];
            } else if word == "feature"
                && let Some(value) = after.strip_prefix('=')
                && let Some(value) = value.trim_start().strip_prefix('"')
                && let Some(close) = value.find('"')
            {
                if !groups.contains(&true) {
                    features.insert(value[..close].to_owned());
                }
                rest = &value[close + 1..];
            }
        } else {
            rest = &rest[character.len_utf8()..];
        }
    }
    features
}

fn declared_module(line: &str) -> Option<(String, bool)> {
    let words: Vec<&str> = tokens(line).collect();
    let index = words.iter().position(|word| *word == "mod")?;
    if words[..index].iter().any(|word| {
        !matches!(
            *word,
            "pub" | "crate" | "super" | "self" | "in" | "unsafe" | "default"
        )
    }) {
        return None;
    }
    let name = (*words.get(index + 1)?).to_owned();
    let keyword = line.find("mod")?;
    let tail = line[keyword + "mod".len()..].trim_start();
    let tail = tail.strip_prefix(&name)?.trim_start();
    if tail.starts_with('{') {
        Some((name, true))
    } else if tail.starts_with(';') {
        Some((name, false))
    } else {
        None
    }
}

fn declared_function(line: &str) -> Option<String> {
    let words: Vec<&str> = tokens(line).collect();
    let index = words.iter().position(|word| *word == "fn")?;
    if words[..index].iter().any(|word| {
        !matches!(
            *word,
            "pub" | "crate" | "super" | "self" | "in" | "async" | "unsafe" | "const" | "extern"
        )
    }) {
        return None;
    }
    Some((*words.get(index + 1)?).to_owned())
}

/// Whether a line of code holds the `static` keyword (an item, whatever its
/// attributes and visibility), as opposed to the `'static` lifetime.
fn has_static_keyword(line: &str) -> bool {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    line.match_indices("static").any(|(at, keyword)| {
        let before = line[..at].chars().next_back();
        let after = line[at + keyword.len()..].chars().next();
        !before.is_some_and(|character| is_word(character) || character == '\'')
            && !after.is_some_and(is_word)
    })
}
