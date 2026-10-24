//! Call-syntax sugar (string, table and method arguments without
//! parentheses, dotted method declarations) and table-constructor parsing.

use crate::support::eval_number;

#[test]
fn unparenthesized_string_call_is_supported() {
    let result = eval_number(
        r#"
        local function wrap(s) return s end
        return wrap "a" .. "b" == "ab" and 1 or 0
    "#,
    );
    assert_eq!(result, 1.0);
}

#[test]
fn unparenthesized_table_call_is_supported() {
    let result = eval_number(
        r#"
        local function get(t) return t.value end
        return get { value = 41 } + 1
    "#,
    );
    assert_eq!(result, 42.0);
}

#[test]
fn unparenthesized_method_call_is_supported() {
    let result = eval_number(
        r#"
        local obj = {
            base = 9,
            plus = function(self, t) return self.base + t.delta end
        }
        return obj:plus { delta = 4 }
    "#,
    );
    assert_eq!(result, 13.0);
}

#[test]
fn table_constructor_accepts_identifier_array_entries() {
    let result = eval_number(
        r#"
        local x = 7
        local t = {x, x + 1}
        return t[1] * 10 + t[2]
    "#,
    );
    assert_eq!(result, 78.0);
}

#[test]
fn table_constructor_distinguishes_named_fields_from_identifiers() {
    let result = eval_number(
        r#"
        local x = 7
        local src = { value = 5 }
        local t = { x = 3, x, src.value }
        return t.x * 100 + t[1] * 10 + t[2]
    "#,
    );
    assert_eq!(result, 375.0);
}

#[test]
fn dotted_method_declaration_is_supported() {
    let result = eval_number(
        r#"
        local mod = { sub = { base = 5 } }
        function mod.sub:add(x)
            return self.base + x
        end
        return mod.sub:add(7)
    "#,
    );
    assert_eq!(result, 12.0);
}
