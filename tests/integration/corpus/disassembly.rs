use dellingr::Program;

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
