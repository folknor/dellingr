use dellingr::Program;

use crate::support::examples;

/// Every example compiles, so every example must disassemble into well-formed
/// rows: function headers, indented detail lines and blank separators, with no
/// out-of-range annotation (finalized bytecode is verified).
#[test]
fn every_example_disassembles_into_well_formed_rows() {
    let files = examples::example_corpus();
    assert!(!files.is_empty(), "the example corpus should not be empty");
    for path in files {
        let source = std::fs::read(&path).expect("example should be readable");
        let name = path.display().to_string();
        let program =
            Program::compile(&source, Some(name.as_bytes())).expect("example should compile");
        let text = program.disassemble().to_string();
        assert!(text.starts_with("function main "), "{}", path.display());
        for line in text.lines() {
            assert!(
                line.is_empty() || line.starts_with("function main") || line.starts_with("  "),
                "{}: malformed listing line {line:?}",
                path.display()
            );
            assert!(
                !line.contains("<out of range>"),
                "{}: {line:?}",
                path.display()
            );
        }
    }
}

#[test]
fn local_count_label_distinguishes_parameters() {
    let program = Program::compile("return function(a, b) local c = a; return b, c end", None)
        .expect("parameter and local fixture compiles");
    let text = program.disassemble().to_string();
    assert!(
        text.contains("params=2 vararg=false non_param_locals=1"),
        "{text}"
    );
}
