//! Optional standard-library arguments use the same defaults when nil or absent.

use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support;

#[derive(Debug, Eq, PartialEq)]
enum LuaResult {
    Nil,
    Boolean(bool),
    Number(u64),
    Bytes(Vec<u8>),
}

struct OptionalArgumentCase {
    name: &'static str,
    with_nil: &'static str,
    absent: &'static str,
}

fn evaluate(source: &str) -> (Vec<LuaResult>, u64) {
    let mut state = State::new();
    support::load(&mut state, source);
    state
        .call(ArgCount::Fixed(0), RetCount::All)
        .unwrap_or_else(|error| panic!("test source must run:\n{source}\n{error}"));

    let count = state.get_top();
    let mut results = Vec::with_capacity(count);
    for index in 1..=count {
        let index = isize::try_from(index).expect("test result index fits on the stack");
        results.push(match state.typ(index) {
            LuaType::Nil => LuaResult::Nil,
            LuaType::Boolean => LuaResult::Boolean(state.to_boolean(index)),
            LuaType::Number => LuaResult::Number(
                state
                    .to_number(index)
                    .expect("number result converts to a number")
                    .to_bits(),
            ),
            LuaType::String => LuaResult::Bytes(
                state
                    .to_bytes(index)
                    .expect("string result converts to bytes")
                    .to_vec(),
            ),
            typ => panic!("optional-argument case returned unsupported {typ} value"),
        });
    }

    (results, state.cost_used())
}

#[test]
fn optional_library_arguments_match_when_nil_or_absent() {
    let cases = [
        OptionalArgumentCase {
            name: "string.sub j",
            with_nil: r#"return string.sub("abc", 2, nil)"#,
            absent: r#"return string.sub("abc", 2)"#,
        },
        OptionalArgumentCase {
            name: "string.find init",
            with_nil: r#"return string.find("zab", "b", nil)"#,
            absent: r#"return string.find("zab", "b")"#,
        },
        OptionalArgumentCase {
            name: "string.find plain",
            with_nil: r#"return string.find("a.b", ".", 1, nil)"#,
            absent: r#"return string.find("a.b", ".", 1)"#,
        },
        OptionalArgumentCase {
            name: "string.match init",
            with_nil: r#"return string.match("zab", "[ab]", nil)"#,
            absent: r#"return string.match("zab", "[ab]")"#,
        },
        OptionalArgumentCase {
            name: "string.gmatch init",
            with_nil: r#"
                local iter = string.gmatch("zab", "[ab]", nil)
                return iter(), iter(), iter(), iter()
            "#,
            absent: r#"
                local iter = string.gmatch("zab", "[ab]")
                return iter(), iter(), iter(), iter()
            "#,
        },
        OptionalArgumentCase {
            name: "string.gsub n",
            with_nil: r#"return string.gsub("aaa", "a", "b", nil)"#,
            absent: r#"return string.gsub("aaa", "a", "b")"#,
        },
        OptionalArgumentCase {
            name: "table.concat sep",
            with_nil: r#"return table.concat({"a", "b"}, nil)"#,
            absent: r#"return table.concat({"a", "b"})"#,
        },
        OptionalArgumentCase {
            name: "table.concat i",
            with_nil: r#"return table.concat({"a", "b", "c"}, "", nil)"#,
            absent: r#"return table.concat({"a", "b", "c"}, "")"#,
        },
        OptionalArgumentCase {
            name: "table.concat j",
            with_nil: r#"return table.concat({"a", "b", "c"}, "", 2, nil)"#,
            absent: r#"return table.concat({"a", "b", "c"}, "", 2)"#,
        },
        OptionalArgumentCase {
            name: "unpack i",
            with_nil: r#"return unpack({"a", "b", "c"}, nil)"#,
            absent: r#"return unpack({"a", "b", "c"})"#,
        },
        OptionalArgumentCase {
            name: "unpack j",
            with_nil: r#"return unpack({"a", "b", "c"}, 2, nil)"#,
            absent: r#"return unpack({"a", "b", "c"}, 2)"#,
        },
        OptionalArgumentCase {
            name: "table.unpack i",
            with_nil: r#"return table.unpack({"a", "b", "c"}, nil)"#,
            absent: r#"return table.unpack({"a", "b", "c"})"#,
        },
        OptionalArgumentCase {
            name: "table.unpack j",
            with_nil: r#"return table.unpack({"a", "b", "c"}, 2, nil)"#,
            absent: r#"return table.unpack({"a", "b", "c"}, 2)"#,
        },
        OptionalArgumentCase {
            name: "table.remove pos",
            with_nil: r#"return table.remove({10, 20, 30}, nil)"#,
            absent: r#"return table.remove({10, 20, 30})"#,
        },
        OptionalArgumentCase {
            name: "tonumber base",
            with_nil: r#"return tonumber("0x10", nil)"#,
            absent: r#"return tonumber("0x10")"#,
        },
    ];

    for case in cases {
        let with_nil = evaluate(case.with_nil);
        let absent = evaluate(case.absent);

        assert_eq!(
            with_nil.0, absent.0,
            "{} results differ: nil call {:?}; absent call {:?}",
            case.name, case.with_nil, case.absent
        );
        assert_eq!(
            with_nil.1, absent.1,
            "{} cost differs: nil call {:?}; absent call {:?}",
            case.name, case.with_nil, case.absent
        );
    }
}
