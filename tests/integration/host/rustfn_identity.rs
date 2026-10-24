use crate::support;

#[test]
fn rust_functions_have_value_identity_in_lua() {
    assert!(support::eval_boolean(
        r#"
        local alias = print
        return print == print
            and alias == print
            and rawequal(print, print)
            and print ~= type
        "#,
    ));
}

#[test]
fn rust_functions_work_as_inline_and_promoted_table_keys() {
    assert!(support::eval_boolean(
        r#"
        local inline = {}
        inline[print] = "first"
        inline[print] = "second"

        local promoted = { a = 1, b = 2, c = 3, d = 4 }
        promoted[print] = "first"
        promoted[print] = "second"

        return inline[print] == "second" and promoted[print] == "second"
        "#,
    ));
}

#[test]
fn reassigning_a_rust_function_key_does_not_append_a_duplicate() {
    assert!(support::eval_boolean(
        r#"
        local table = {}
        table[print] = "first"
        table[print] = "second"

        local count = 0
        for key, value in pairs(table) do
            if key == print and value == "second" then
                count = count + 1
            end
        end
        return count == 1
        "#,
    ));
}
