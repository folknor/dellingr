//! Where code generation closes captured locals: at block, `if` arm, loop
//! iteration and `break` exits, before the slot is reused.

use crate::support;

#[test]
fn block_upvalue_closes_before_slot_reuse() {
    assert_eq!(
        support::eval_number(
            "do local x = 1; f = function() return x end end; local x = 2; return f()"
        ),
        1.0
    );
}

#[test]
fn if_arm_upvalue_closes_and_false_branch_reaches_next_arm() {
    assert_eq!(
        support::eval_number(
            "if false then local x = 1; f = function() return x end elseif true then local x = 2; f = function() return x end else local x = 3; f = function() return x end end; local x = 4; return f()"
        ),
        2.0
    );
}

#[test]
fn numeric_for_closes_visible_variable_each_iteration() {
    assert_eq!(
        support::eval_number(
            "t = {}; for i = 1, 3 do t[i] = function() return i end end; return t[1]() * 100 + t[2]() * 10 + t[3]()"
        ),
        123.0
    );
}

#[test]
fn generic_for_closes_all_visible_variables_each_iteration() {
    assert_eq!(
        support::eval_number(
            "t = {}; for k, v in ipairs({10, 20, 30}) do t[k] = function() return k * 100 + v end end; return t[1]() + t[2]() + t[3]()"
        ),
        660.0
    );
}

#[test]
fn break_closes_nested_loop_local_before_slot_reuse() {
    assert_eq!(
        support::eval_number(
            "while true do do local x = 7; f = function() return x end; break end end; local x = 8; return f()"
        ),
        7.0
    );
}

#[test]
fn while_iterations_keep_outer_upvalue_shared() {
    assert_eq!(
        support::eval_number(
            "local counter = 0; while counter < 3 do counter = counter + 1; if counter == 1 then f = function() return counter end end end; return f() * 10 + counter"
        ),
        33.0
    );
}

// Plain `if ... then ... end` with no following arm: the taken branch closes
// its captured local via the level_down in close_if_arm's no-next-arm path.
#[test]
fn plain_if_arm_closes_captured_local_before_slot_reuse() {
    assert_eq!(
        support::eval_number(
            "if true then local x = 1; f = function() return x end end; local x = 2; return f()"
        ),
        1.0
    );
}

// break capturing a direct while-body local (not nested in a do): break's
// pre-jump close handles it, and the loop's final level_down emits a second,
// harmless close at the same base.
#[test]
fn break_closes_direct_while_local_before_slot_reuse() {
    assert_eq!(
        support::eval_number(
            "while true do local x = 5; f = function() return x end; break end; local x = 6; return f()"
        ),
        5.0
    );
}
