use dellingr::{ArgCount, RetCount, State};

#[test]
fn literal_return_and_unpack_keep_all_255_values() {
    let values = (1..=255)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    for source in [
        format!("return {values}"),
        format!("return table.unpack({{{values}}})"),
    ] {
        let mut state = State::new();
        state
            .load_string(&source)
            .expect("255-result source compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(255))
            .expect("255 results run");
        assert_eq!(state.get_top(), 255);
        assert_eq!(state.to_number(1).expect("first result is numeric"), 1.0);
        assert_eq!(state.to_number(255).expect("last result is numeric"), 255.0);
    }
}

#[test]
fn host_call_accepts_255_fixed_arguments() {
    let mut state = State::new();
    state
        .load_string("return function(...) return ... end")
        .expect("vararg function compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("function is returned");
    for n in 1..=255 {
        state
            .push_number(f64::from(n))
            .expect("argument fits the stack");
    }
    state
        .call(ArgCount::Fixed(255), RetCount::All)
        .expect("host count is not a bytecode sentinel");
    assert_eq!(state.get_top(), 255);
    assert_eq!(
        state.to_number(255).expect("last argument is returned"),
        255.0
    );
}

#[test]
fn callable_table_receiver_can_make_255_runtime_arguments() {
    let args = vec!["1"; 254].join(",");
    let source = format!(
        "local t = setmetatable({{}}, {{__call = function(...) local packed = {{...}}; return #packed end}}); return t({args})"
    );
    let mut state = State::new();
    state
        .load_string(&source)
        .expect("254 explicit arguments compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("receiver is a runtime argument");
    assert_eq!(
        state.to_number(1).expect("argument count is numeric"),
        255.0
    );
}
