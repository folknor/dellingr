//! The bare global names a program's source spells, recorded by the parser
//! where each identifier resolves ([`crate::Program::globals`]).

/// Whether an occurrence reads or assigns the global.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AccessKind {
    /// `x` used as a value, including the table of `x.f = v` or
    /// `function x.f() end`.
    Read,
    /// `x` as a whole assignment target: `x = v`, one of `x, y = ...`, or
    /// `function x() end`.
    Write,
}

/// One occurrence of a free identifier that names a global.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct GlobalAccess {
    /// The global's name, byte-exact.
    pub name: Box<[u8]>,
    /// The identifier's one-based source line.
    pub line: u32,
    /// The identifier's one-based column, in bytes.
    pub column: u32,
    /// Read or write.
    pub kind: AccessKind,
}

#[cfg(test)]
mod tests {
    use super::{AccessKind, GlobalAccess};
    use crate::Program;

    use AccessKind::{Read, Write};

    fn accesses(source: &str) -> Vec<(String, u32, u32, AccessKind)> {
        let program = Program::compile(source, None).unwrap();
        program
            .globals()
            .iter()
            .map(|access| {
                let GlobalAccess {
                    name,
                    line,
                    column,
                    kind,
                } = access.clone();
                (
                    String::from_utf8(name.into_vec()).unwrap(),
                    line,
                    column,
                    kind,
                )
            })
            .collect()
    }

    fn row(name: &str, line: u32, column: u32, kind: AccessKind) -> (String, u32, u32, AccessKind) {
        (name.into(), line, column, kind)
    }

    #[test]
    fn occurrences_are_listed_in_lexical_order() {
        let got = accesses("x = function()\n  return typo\nend\nlater()\n");
        assert_eq!(
            got,
            vec![
                row("x", 1, 1, Write),
                row("typo", 2, 10, Read),
                row("later", 4, 1, Read)
            ]
        );
    }

    #[test]
    fn a_write_takes_its_identifier_line_and_column() {
        let got = accesses("x =\n  f()\n");
        assert_eq!(got, vec![row("x", 1, 1, Write), row("f", 2, 3, Read)]);
    }

    #[test]
    fn only_a_bare_target_is_a_write() {
        let got = accesses("x[x] = x\n");
        assert_eq!(
            got,
            vec![
                row("x", 1, 1, Read),
                row("x", 1, 3, Read),
                row("x", 1, 8, Read)
            ]
        );
    }

    #[test]
    fn a_multiple_assignment_marks_each_bare_target() {
        let got = accesses("a, t[b] = c, d\n");
        assert_eq!(
            got,
            vec![
                row("a", 1, 1, Write),
                row("t", 1, 4, Read),
                row("b", 1, 6, Read),
                row("c", 1, 11, Read),
                row("d", 1, 14, Read),
            ]
        );
    }

    #[test]
    fn a_function_declaration_writes_its_name_before_its_body() {
        let got = accesses("function f()\n  return f\nend\n");
        assert_eq!(got, vec![row("f", 1, 10, Write), row("f", 2, 10, Read)]);
    }

    #[test]
    fn a_dotted_or_method_declaration_reads_its_table() {
        let got = accesses("function t.m() end\nfunction\n  u.v:n() end\n");
        assert_eq!(got, vec![row("t", 1, 10, Read), row("u", 3, 3, Read)]);
    }

    #[test]
    fn builtin_slots_are_listed_like_any_global() {
        let got = accesses("local a = tostring(1)\nlocal env = _G\nprint = nil\n");
        assert_eq!(
            got,
            vec![
                row("tostring", 1, 11, Read),
                row("_G", 2, 13, Read),
                row("print", 3, 1, Write),
            ]
        );
    }

    #[test]
    fn locals_upvalues_and_fields_are_not_listed() {
        let got = accesses(
            "local function f() f() end\nlocal x = 1\nlocal t = {y = x}\n\
             function g() return x, f, t.z end\nlocal print = print\nprint()\n",
        );
        assert_eq!(got, vec![row("g", 4, 10, Write), row("print", 5, 15, Read)]);
    }

    #[test]
    fn a_binding_resolves_its_initializer_before_it_is_in_scope() {
        let got = accesses("local x = x\nfor i = i, limit do end\nfor k in iter(k) do end\n");
        assert_eq!(
            got,
            vec![
                row("x", 1, 11, Read),
                row("i", 2, 9, Read),
                row("limit", 2, 12, Read),
                row("iter", 3, 10, Read),
                row("k", 3, 15, Read),
            ]
        );
    }

    #[test]
    fn dead_branches_keep_their_occurrences() {
        let got = accesses("if false then\n  typo()\nend\n");
        assert_eq!(got, vec![row("typo", 2, 3, Read)]);
    }

    #[test]
    fn columns_count_bytes_past_comments_tabs_and_multibyte_strings() {
        let got = accesses("-- c\n\tx = '\u{e9}' .. y\r\nz()\n");
        assert_eq!(
            got,
            vec![
                row("x", 2, 2, Write),
                row("y", 2, 14, Read),
                row("z", 3, 1, Read)
            ]
        );
    }

    #[test]
    fn a_multiline_basic_header_writes_its_name() {
        let got = accesses("function\n  f() return f end\n");
        assert_eq!(got, vec![row("f", 2, 3, Write), row("f", 2, 14, Read)]);
    }

    #[test]
    fn every_bare_target_of_a_multiple_assignment_is_a_write() {
        let got = accesses("a, b = c, d\n");
        assert_eq!(
            got,
            vec![
                row("a", 1, 1, Write),
                row("b", 1, 4, Write),
                row("c", 1, 8, Read),
                row("d", 1, 11, Read)
            ]
        );
    }

    #[test]
    fn parenthesized_and_call_receivers_stay_reads() {
        let got = accesses("(x).f = y\nfactory()[key] = value\n");
        assert_eq!(
            got,
            vec![
                row("x", 1, 2, Read),
                row("y", 1, 9, Read),
                row("factory", 2, 1, Read),
                row("key", 2, 11, Read),
                row("value", 2, 18, Read),
            ]
        );
    }

    #[test]
    fn records_outlive_the_source_buffer() {
        let source = String::from("x = y\n");
        let program = Program::compile(source.as_bytes(), None).unwrap();
        drop(source);
        let names: Vec<&[u8]> = program.globals().iter().map(|a| &*a.name).collect();
        assert_eq!(names, [&b"x"[..], &b"y"[..]]);
    }
}
