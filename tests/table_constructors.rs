use dellingr::{ArgCount, RetCount, State};

fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

#[test]
fn templated_constructor_does_not_iterate_nil_fields() {
    let result = run_number(
        r#"
        local t = {a = nil, b = 2, c = 3, d = 4, e = 5}
        local count = 0
        local sum = 0
        local saw_a = 0
        for k, v in pairs(t) do
            count = count + 1
            sum = sum + v
            if k == "a" then
                saw_a = 1
            end
        end
        return count * 100 + sum + saw_a
    "#,
    );
    assert_eq!(result, 414.0);
}

#[test]
fn templated_constructor_handles_nil_before_later_fields() {
    let result = run_number(
        r#"
        local t = {a = 1, b = nil, c = 3, d = 4, e = 5}
        local count = 0
        local sum = 0
        local saw_b = 0
        for k, v in pairs(t) do
            count = count + 1
            sum = sum + v
            if k == "b" then
                saw_b = 1
            end
        end
        return count * 100 + sum + saw_b
    "#,
    );
    assert_eq!(result, 413.0);
}

#[test]
fn duplicate_named_constructor_fields_keep_last_value() {
    let result = run_number(
        r#"
        local t = {a = 1, b = 2, c = 3, d = 4, a = 5}
        return t.a * 1000 + t.b * 100 + t.c * 10 + t.d
    "#,
    );
    assert_eq!(result, 5234.0);
}
