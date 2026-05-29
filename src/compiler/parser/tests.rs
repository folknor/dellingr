use std::sync::Arc;

use super::Bytecode;
use super::Instr;
use super::parse_str;
use crate::instr::{ArgCount, Builtin, RetCount};

/// Recursively clear line_info from a chunk and its nested chunks.
fn clear_line_info(chunk: &mut Bytecode) {
    chunk.line_info.clear();
    for nested in &mut chunk.nested {
        let inner = Arc::get_mut(nested).expect("test fixture should own its nested chunks");
        clear_line_info(inner);
    }
}

fn check_it(input: &str, mut output: Bytecode) {
    // Top-level chunks are always vararg functions
    output.is_vararg = true;
    let mut actual = parse_str(input).unwrap();
    // Clear line_info for comparison (tests were written before line tracking existed)
    clear_line_info(&mut actual);
    assert_eq!(actual, output);
}

#[test]
fn test01() {
    let text = "x = 5 + 6";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::add(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0, 6.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test02() {
    let text = "x = -5^2";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::pow(),
            Instr::negate(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0, 2.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test03() {
    let text = "x = 5 + true .. 'hi'";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_bool(true),
            Instr::add(),
            Instr::push_string(1),
            Instr::concat(2),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0],
        string_literals: vec!["x".into(), "hi".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test04() {
    let text = "x = 1 .. 2 + 3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::push_num(2),
            Instr::add(),
            Instr::concat(2),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![1.0, 2.0, 3.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn concat_chain_emits_single_n_ary_concat() {
    let text = r#"x = "a" .. "b" .. "c" .. "d""#;
    let output = Bytecode {
        code: vec![
            Instr::push_string(1),
            Instr::push_string(2),
            Instr::push_string(3),
            Instr::push_string(4),
            Instr::concat(4),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test05() {
    let text = "x = 2^-3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::negate(),
            Instr::pow(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![2.0, 3.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test06() {
    let text = "x=  not not 1";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::not(),
            Instr::not(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![1.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test07() {
    let text = "a = 5";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test08() {
    let text = "x = true and false";
    let output = Bytecode {
        code: vec![
            Instr::push_bool(true),
            Instr::branch_false_keep(2),
            Instr::pop(),
            Instr::push_bool(false),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test09() {
    let text = "x =  5 or nil and true";
    let code = vec![
        Instr::push_num(0),
        Instr::branch_true_keep(5),
        Instr::pop(),
        Instr::push_nil(),
        Instr::branch_false_keep(2),
        Instr::pop(),
        Instr::push_bool(true),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let output = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test10() {
    let text = "if true then a = 5 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(2),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test11() {
    let text = "if true then a = 5 if true then b = 4 end end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(6),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::push_bool(true),
        Instr::branch_false(2),
        Instr::push_num(1),
        Instr::set_global(1),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test12() {
    let text = "if true then a = 5 else a = 4 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::jump(2),
        Instr::push_num(1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test13() {
    let text = "if true then a = 5 elseif 6 == 7 then a = 3 else a = 4 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::jump(9),
        Instr::push_num(1),
        Instr::push_num(2),
        Instr::equal(),
        Instr::branch_false(3),
        Instr::push_num(3),
        Instr::set_global(0),
        Instr::jump(2),
        Instr::push_num(4),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 6.0, 7.0, 3.0, 4.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test14() {
    let text = "while a < 10 do a = a + 1 end";
    let code = vec![
        Instr::get_global(0),
        Instr::push_num(0),
        Instr::less(),
        Instr::branch_false(5),
        Instr::get_global(0),
        Instr::push_num(1),
        Instr::add(),
        Instr::set_global(0),
        Instr::jump(-9),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![10.0, 1.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test15() {
    // repeat local x = 5 until a == b - body locals get CloseUpvalues before jump
    let text = "repeat local x = 5 until a == b y = 4";
    let code = vec![
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::get_global(0),
        Instr::get_global(1),
        Instr::equal(),
        Instr::close_upvalues(0), // close body-local x before potential jump back
        Instr::branch_false(-7),
        Instr::push_num(1),
        Instr::set_global(2),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into(), "b".into(), "y".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test16() {
    let text = "local i i = 2";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![2.0],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test17() {
    let text = "local i, j print(j)";
    let code = vec![
        Instr::push_nil(),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::set_local(0),
        Instr::get_builtin(Builtin::Print),
        Instr::get_local(1),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test18() {
    let text = "local i do local i x = i end x = i";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::get_local(1),
        Instr::set_global(0),
        Instr::get_local(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into()],
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test19() {
    let text = "do local i x = i end x = i";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::get_local(0),
        Instr::set_global(0),
        Instr::get_global(1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into(), "i".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test20() {
    let text = "local i if false then local i else x = i end";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_bool(false),
        Instr::branch_false(3),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::jump(2),
        Instr::get_local(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into()],
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test21() {
    let text = "for i = 1,5 do x = i end";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::push_num(0),
        Instr::for_prep(0, 3),
        Instr::get_local(3),
        Instr::set_global(0),
        Instr::for_loop(0, -3),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 5.0],
        string_literals: vec!["x".into()],
        num_locals: 4,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test22() {
    let text = "a, b = 1";
    let code = vec![
        Instr::push_num(0),
        Instr::push_nil(),
        Instr::set_global(1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test23() {
    let text = "a, b = 1, 2";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::set_global(1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test24() {
    let text = "a, b = 1, 2, 3";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::push_num(2),
        Instr::pop(),
        Instr::set_global(1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test25() {
    let text = "puts()";
    let code = vec![
        Instr::get_global(0),
        Instr::call(ArgCount::Fixed(0), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["puts".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test26() {
    let text = "y = {x = 5,}";
    let code = vec![
        Instr::new_table(),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["y".into(), "x".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn table_constructor_presizes_larger_literals() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, e = 5}";
    let code = vec![
        Instr::new_table_template(0),
        Instr::push_num(0),
        Instr::init_field_pinned(1, 0),
        Instr::push_num(1),
        Instr::init_field_pinned(2, 1),
        Instr::push_num(2),
        Instr::init_field_pinned(3, 2),
        Instr::push_num(3),
        Instr::init_field_pinned(4, 3),
        Instr::push_num(4),
        Instr::init_field_pinned(5, 4),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec![
            "y".into(),
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
        ],
        table_templates: vec![vec![1, 2, 3, 4, 5]],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn duplicate_table_constructor_keys_use_presized_table() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, a = 5}";
    let code = vec![
        Instr::new_table_presized(5),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::push_num(1),
        Instr::init_field(0, 2),
        Instr::push_num(2),
        Instr::init_field(0, 3),
        Instr::push_num(3),
        Instr::init_field(0, 4),
        Instr::push_num(4),
        Instr::init_field(0, 1),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec!["y".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn computed_table_constructor_keys_use_presized_table() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, [k] = 5}";
    let code = vec![
        Instr::new_table_presized(5),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::push_num(1),
        Instr::init_field(0, 2),
        Instr::push_num(2),
        Instr::init_field(0, 3),
        Instr::push_num(3),
        Instr::init_field(0, 4),
        Instr::get_global(5),
        Instr::push_num(4),
        Instr::init_index(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec![
            "y".into(),
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "k".into(),
        ],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test27() {
    let text = "local x = t.x.y";
    let code = vec![
        Instr::get_global(0),
        Instr::get_field(1),
        Instr::get_field(2),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["t".into(), "x".into(), "y".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test28() {
    let text = "x = function () end";
    let code = vec![
        Instr::closure(0),
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let string_literals = vec!["x".into()];
    let nested = vec![Arc::new(Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
        ..Bytecode::default()
    })];
    let chunk = Bytecode {
        code,
        string_literals,
        nested,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test29() {
    let text = "x = function () local y = 7 end";
    let inner_chunk = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![7.0],
        num_locals: 1,
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into()],
        nested: vec![Arc::new(inner_chunk)],
        ..Bytecode::default()
    };
    check_it(text, outer_chunk);
}

#[test]
fn test30() {
    let text = "
    z = function () local z = 21 end
    x = function ()
        local y = function () end
        print(y)
    end";
    let z = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![21.0],
        num_locals: 1,
        ..Bytecode::default()
    };
    let y = Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
        ..Bytecode::default()
    };
    let x = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_local(0),
            Instr::get_builtin(Builtin::Print),
            Instr::get_local(0),
            Instr::call(ArgCount::Fixed(1), RetCount::Fixed(0)),
            Instr::ret(RetCount::Fixed(0)),
        ],
        nested: vec![Arc::new(y)],
        num_locals: 1,
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0),
            Instr::closure(1),
            Instr::set_global(1),
            Instr::ret(RetCount::Fixed(0)),
        ],
        nested: vec![Arc::new(z), Arc::new(x)],
        string_literals: vec!["z".into(), "x".into()],
        ..Bytecode::default()
    };
    check_it(text, outer_chunk);
}

#[test]
fn test31() {
    let text = "local s = type(4)";
    let code = vec![
        Instr::get_builtin(Builtin::Type),
        Instr::push_num(0),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(1)),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 1,
        number_literals: vec![4.0],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test32() {
    // When the last argument is a function call, all its return values are passed
    let text = "local type, print print(type(nil))";
    let code = vec![
        Instr::push_nil(),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::set_local(0),
        Instr::mark_call_base(0), // Mark stack position before function (no adjustment, not a field access)
        Instr::get_local(1),      // Get print
        Instr::get_local(0),      // Get type
        Instr::push_nil(),        // Push nil argument
        Instr::call(ArgCount::Fixed(1), RetCount::All), // Call type(nil), return ALL values
        Instr::call(ArgCount::Dynamic, RetCount::Fixed(0)), // Call print with dynamic arg count
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test33() {
    use super::*;
    let text = "print()\n(foo)()\n";
    match parse_str(text) {
        Err(Error {
            kind: ErrorKind::SyntaxError(SyntaxError::LParenLineStart),
            line_num,
            column,
            ..
        }) => {
            assert_eq!(line_num, 2);
            assert_eq!(column, 1);
        }
        _ => panic!("Should detect ambiguous function call because of linebreak"),
    }
}

#[test]
fn test34() {
    // while false do local b end - body locals get CloseUpvalues before jump
    let text = "while false do local b end b()";
    let code = vec![
        Instr::push_bool(false),
        Instr::branch_false(4),
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::close_upvalues(0), // close body-local b before jump back
        Instr::jump(-6),
        Instr::get_global(0),
        Instr::call(ArgCount::Fixed(0), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 1,
        string_literals: vec!["b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}
