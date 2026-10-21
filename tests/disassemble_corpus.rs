#[path = "common/examples.rs"]
mod example_files;

use dellingr::Engine;

/// Every example compiles, so every example must disassemble into well-formed
/// rows: function headers, indented detail lines and blank separators, with no
/// out-of-range annotation (finalized bytecode is verified).
#[test]
fn every_example_disassembles_into_well_formed_rows() {
    let files = example_files::example_corpus();
    assert!(!files.is_empty(), "the example corpus should not be empty");
    let engine = Engine::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("example should be readable");
        let program = engine
            .compile_named(&source, path.display().to_string())
            .expect("example should compile");
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
