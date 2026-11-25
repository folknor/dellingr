use crate::support;

#[test]
fn templated_constructor_does_not_iterate_nil_fields() {
    let result = support::eval_number(
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
    let result = support::eval_number(
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

/// A template constructor whose values are all locals, upvalues and literals
/// runs its pinned writes in one call after the allocation. Each write still
/// admits its own charge at its own line: a budget refused at the k-th field
/// fails at that field's line with the same `cost_used` as one dispatch per
/// write.
#[test]
fn fused_template_writes_charge_and_fail_at_their_own_fields() {
    use dellingr::error::ErrorKind;
    use dellingr::{ArgCount, RetCount, State};

    let code = "local x, y = 1, 2\n\
                local t = {\n\
                a = x,\n\
                b = y,\n\
                c = 3,\n\
                d = true,\n\
                e = 'five',\n\
                }\n\
                return t.a + t.b + t.c";
    // NewTable at line 2, then one TableWrite per field on lines 3 to 7.
    for budget in 0..=5u32 {
        let mut state = State::new();
        dellingr::Program::compile(code, Some(b"fused".as_slice()))
            .and_then(|program| state.load(&program))
            .unwrap();
        state.set_cost_budget(i64::from(budget)).unwrap();
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect_err("the budget ends inside the constructor");
        assert!(
            matches!(error.kind(), ErrorKind::BudgetExceeded { used, .. } if *used == u64::from(budget)),
            "budget {budget}: {error:?}"
        );
        assert_eq!(state.cost_used(), u64::from(budget), "budget {budget}");
        assert_eq!(error.stack_trace()[0].line, 2 + budget, "budget {budget}");
    }
}

#[test]
fn duplicate_named_constructor_fields_keep_last_value() {
    let result = support::eval_number(
        r#"
        local t = {a = 1, b = 2, c = 3, d = 4, a = 5}
        return t.a * 1000 + t.b * 100 + t.c * 10 + t.d
    "#,
    );
    assert_eq!(result, 5234.0);
}
