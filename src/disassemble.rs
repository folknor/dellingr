//! Human-readable listing of compiled bytecode, the engine behind
//! [`Program::disassemble`](crate::Program::disassemble) and the CLI's
//! `--dump-bytecode` flag.
//!
//! The listing shows finalized bytecode - after cache-slot assignment, so
//! exactly what executes. It is diagnostic text: deterministic for a given
//! compiled program, but its format carries no cross-version guarantee and it
//! is not replay data.
//!
//! The annotations describe the operands of `instr.rs`: literal-pool
//! operands, cache slots, counts and transfer targets. Cache slots and counts
//! are read through `Instr`'s typed decoders, so they cannot drift from the
//! encoding; the unit tests below pin the rendering.

use std::fmt;

use crate::compiler::{Bytecode, UpvalueDesc};
use crate::instr::{ArgCount, ArithOp, ArithOperands, Instr, RetCount};

pub(crate) const OUT_OF_RANGE_ANNOTATION: &str = "<out of range>";

/// A borrowed, displayable bytecode listing of a [`Program`](crate::Program).
///
/// Returned by [`Program::disassemble`](crate::Program::disassemble). The
/// output is deterministic for a given compiled program; its format is
/// diagnostic and may change between versions.
pub struct Disassembly<'a> {
    root: &'a Bytecode,
}

impl<'a> Disassembly<'a> {
    pub(crate) fn new(root: &'a Bytecode) -> Self {
        Self { root }
    }
}

impl fmt::Display for Disassembly<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut path = String::from("main");
        write_function(f, self.root, &mut path)
    }
}

/// Writes `text` as a double-quoted, byte-escaped string, so names, sources
/// and literals cannot inject control characters or fake listing rows.
fn write_quoted(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    write!(f, "\"{}\"", bytes.escape_ascii())
}

fn write_function(f: &mut fmt::Formatter<'_>, bc: &Bytecode, path: &mut String) -> fmt::Result {
    write!(f, "function {path} name=")?;
    match &bc.name {
        Some(name) => write_quoted(f, name.as_bytes())?,
        None => f.write_str("-")?,
    }
    f.write_str(" source=")?;
    match &bc.source {
        Some(source) => write_quoted(f, source)?,
        None => f.write_str("-")?,
    }
    writeln!(
        f,
        " params={} vararg={} non_param_locals={} registers={}",
        bc.num_params, bc.is_vararg, bc.num_locals, bc.max_stack
    )?;
    writeln!(
        f,
        "  cache slots: global={} field={} set_field={} tfor_cursor={}",
        bc.global_cache_slots, bc.field_cache_slots, bc.set_field_cache_slots, bc.tfor_cursor_slots
    )?;

    if !bc.upvalues.is_empty() {
        writeln!(f, "  upvalues:")?;
        for (idx, desc) in bc.upvalues.iter().enumerate() {
            match desc {
                UpvalueDesc::Local(slot) => writeln!(f, "    [{idx}] parent local {slot}")?,
                UpvalueDesc::Upvalue(up) => writeln!(f, "    [{idx}] parent upvalue {up}")?,
            }
        }
    }
    if !bc.number_literals.is_empty() {
        writeln!(f, "  numbers:")?;
        for (idx, n) in bc.number_literals.iter().enumerate() {
            writeln!(f, "    [{idx}] {n:?} (0x{:016x})", n.to_bits())?;
        }
    }
    if !bc.string_literals.is_empty() {
        writeln!(f, "  strings:")?;
        for (idx, s) in bc.string_literals.iter().enumerate() {
            write!(f, "    [{idx}] ")?;
            write_quoted(f, s)?;
            writeln!(f)?;
        }
    }
    if !bc.table_templates.is_empty() {
        writeln!(f, "  templates:")?;
        for (idx, keys) in bc.table_templates.iter().enumerate() {
            write!(f, "    [{idx}]")?;
            for key in keys {
                f.write_str(" ")?;
                write_string_ref(f, bc, *key)?;
            }
            writeln!(f)?;
        }
    }

    writeln!(f, "  code:")?;
    for (pc, inst) in bc.code.iter().enumerate() {
        let line = bc.line_info.get(pc).copied().unwrap_or(0);
        write!(f, "    [{pc}] line {line}  {inst:?}")?;
        write_annotation(f, bc, path, pc, *inst)?;
        writeln!(f)?;
    }

    for (idx, child) in bc.nested.iter().enumerate() {
        let len = path.len();
        path.push('/');
        path.push_str(&idx.to_string());
        writeln!(f)?;
        write_function(f, child, path)?;
        path.truncate(len);
    }
    Ok(())
}

fn write_string_ref(f: &mut fmt::Formatter<'_>, bc: &Bytecode, idx: u16) -> fmt::Result {
    match bc.string_literals.get(usize::from(idx)) {
        Some(s) => write_quoted(f, s),
        None => f.write_str(OUT_OF_RANGE_ANNOTATION),
    }
}

/// Separator state for the `; a, b` annotation suffix. Annotations
/// accumulate: one instruction can carry a literal, a cache and a target.
struct Notes {
    started: bool,
}

impl Notes {
    fn next(&mut self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.started { ", " } else { "  ; " })?;
        self.started = true;
        Ok(())
    }
}

fn write_annotation(
    f: &mut fmt::Formatter<'_>,
    bc: &Bytecode,
    path: &str,
    pc: usize,
    inst: Instr,
) -> fmt::Result {
    let mut notes = Notes { started: false };
    let op = inst.opcode();

    let write_number =
        |f: &mut fmt::Formatter<'_>, idx: u16| match bc.number_literals.get(usize::from(idx)) {
            Some(n) => write!(f, "{n:?}"),
            None => f.write_str(OUT_OF_RANGE_ANNOTATION),
        };

    // Literal-pool operands, in the field each opcode keeps them in.
    match op {
        Instr::OP_LOAD_NUM => {
            notes.next(f)?;
            write_number(f, inst.b())?;
        }
        Instr::OP_LOAD_STR
        | Instr::OP_GET_GLOBAL
        | Instr::OP_SET_GLOBAL
        | Instr::OP_SET_FIELD
        | Instr::OP_INIT_FIELD
        | Instr::OP_INIT_FIELD_PINNED => {
            notes.next(f)?;
            write_string_ref(f, bc, inst.b())?;
        }
        Instr::OP_GET_FIELD | Instr::OP_SELF => {
            notes.next(f)?;
            write_string_ref(f, bc, inst.c())?;
        }
        Instr::OP_NEW_TABLE_TEMPLATE => {
            notes.next(f)?;
            write!(f, "template {}", inst.b())?;
        }
        Instr::OP_CLOSURE => {
            notes.next(f)?;
            if usize::from(inst.b()) < bc.nested.len() {
                write!(f, "function {path}/{}", inst.b())?;
            } else {
                write!(f, "function {OUT_OF_RANGE_ANNOTATION}")?;
            }
        }
        _ => match ArithOp::decode(op) {
            Some((_, ArithOperands::RegConst)) => {
                notes.next(f)?;
                write_number(f, inst.c())?;
            }
            Some((_, ArithOperands::ConstReg)) => {
                notes.next(f)?;
                write_number(f, inst.b())?;
            }
            _ => {}
        },
    }

    // Cache operands, decoded by `Instr`'s cache-slot decoder.
    if matches!(
        op,
        Instr::OP_GET_GLOBAL
            | Instr::OP_SET_GLOBAL
            | Instr::OP_GET_FIELD
            | Instr::OP_SELF
            | Instr::OP_SET_FIELD
            | Instr::OP_TFOR_CALL
    ) {
        notes.next(f)?;
        match inst.cache_slot() {
            Some(slot) => write!(f, "cache slot {slot}")?,
            None => f.write_str("uncached")?,
        }
    }

    match op {
        Instr::OP_VARARG if inst.vararg_count() == RetCount::All => {
            notes.next(f)?;
            f.write_str("all values")?;
        }
        Instr::OP_RETURN if inst.return_count() == RetCount::All => {
            notes.next(f)?;
            f.write_str("all values")?;
        }
        Instr::OP_SET_LIST if inst.set_list_count().is_none() => {
            notes.next(f)?;
            f.write_str("open count")?;
        }
        Instr::OP_CALL => {
            if inst.call_args() == ArgCount::Dynamic {
                notes.next(f)?;
                f.write_str("open arguments")?;
            }
            if inst.call_results() == RetCount::All {
                notes.next(f)?;
                f.write_str("all results")?;
            }
            if inst.call_is_method() {
                notes.next(f)?;
                f.write_str("method")?;
            }
        }
        _ => {}
    }

    if Instr::is_transfer(op) {
        notes.next(f)?;
        let target = pc
            .checked_add(1)
            .and_then(|next| next.checked_add_signed(inst.sj() as isize));
        match target {
            Some(target) if target < bc.code.len() => write!(f, "target={target}")?,
            _ => write!(f, "target={OUT_OF_RANGE_ANNOTATION}")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn annotate(bc: &Bytecode, pc: usize) -> String {
        struct One<'a>(&'a Bytecode, usize);
        impl fmt::Display for One<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_annotation(f, self.0, "main", self.1, self.0.code[self.1])
            }
        }
        One(bc, pc).to_string()
    }

    fn with_code(code: Vec<Instr>) -> Bytecode {
        Bytecode {
            line_info: vec![1; code.len()],
            code,
            number_literals: vec![-0.0],
            string_literals: vec![b"x\n\xff".to_vec()],
            table_templates: vec![vec![0]],
            nested: vec![Arc::new(Bytecode::default())],
            ..Bytecode::default()
        }
    }

    #[test]
    fn cache_slots_are_named() {
        let bc = with_code(vec![
            Instr::get_global(0, 0, None),
            Instr::get_global(0, 0, Some(254)),
            Instr::set_global(0, 0, None),
            Instr::set_global(0, 0, Some(254)),
            Instr::get_field(0, 0, 0, None),
            Instr::get_field(0, 0, 0, Some(3)),
            Instr::set_field(0, 0, 0, None),
            Instr::tfor_call(0, 1, None),
            Instr::tfor_call(0, 1, Some(254)),
            Instr::self_(0, 0, 0, Some(5)),
        ]);
        assert_eq!(annotate(&bc, 0), "  ; \"x\\n\\xff\", uncached");
        assert_eq!(annotate(&bc, 1), "  ; \"x\\n\\xff\", cache slot 254");
        assert_eq!(annotate(&bc, 2), "  ; \"x\\n\\xff\", uncached");
        assert_eq!(annotate(&bc, 3), "  ; \"x\\n\\xff\", cache slot 254");
        assert_eq!(annotate(&bc, 4), "  ; \"x\\n\\xff\", uncached");
        assert_eq!(annotate(&bc, 5), "  ; \"x\\n\\xff\", cache slot 3");
        assert_eq!(annotate(&bc, 6), "  ; \"x\\n\\xff\", uncached");
        assert_eq!(annotate(&bc, 7), "  ; uncached");
        assert_eq!(annotate(&bc, 8), "  ; cache slot 254");
        assert_eq!(annotate(&bc, 9), "  ; \"x\\n\\xff\", cache slot 5");
    }

    #[test]
    fn count_sentinels_pools_and_children_are_named() {
        let bc = with_code(vec![
            Instr::vararg(0, RetCount::All),
            Instr::vararg(0, RetCount::Fixed(2)),
            Instr::set_list(0, None, 1),
            Instr::set_list(0, Some(3), 0),
            Instr::load_num(0, 0),
            Instr::load_num(0, 9),
            Instr::closure(0, 0),
            Instr::closure(0, 1),
            Instr::new_table_template(0, 0),
            Instr::ret(0, RetCount::Fixed(0)),
            Instr::call(0, ArgCount::Dynamic, RetCount::All, true),
            Instr::arith(ArithOp::Add, ArithOperands::RegConst, 0, 1, 0),
        ]);
        assert_eq!(annotate(&bc, 0), "  ; all values");
        assert_eq!(annotate(&bc, 1), "");
        assert_eq!(annotate(&bc, 2), "  ; open count");
        assert_eq!(annotate(&bc, 3), "");
        assert_eq!(annotate(&bc, 4), "  ; -0.0");
        assert_eq!(annotate(&bc, 5), "  ; <out of range>");
        assert_eq!(annotate(&bc, 6), "  ; function main/0");
        assert_eq!(annotate(&bc, 7), "  ; function <out of range>");
        assert_eq!(annotate(&bc, 8), "  ; template 0");
        assert_eq!(annotate(&bc, 9), "");
        assert_eq!(annotate(&bc, 10), "  ; open arguments, all results, method");
        assert_eq!(annotate(&bc, 11), "  ; -0.0");
    }

    #[test]
    fn transfer_targets_are_pc_plus_one_plus_offset() {
        let bc = with_code(vec![
            Instr::jump(1),
            Instr::for_prep(0, 2),
            Instr::branch_compare(crate::instr::CompareOp::Lt, 0, 1, -3),
            Instr::for_loop(0, -4),
            Instr::tfor_loop(0, -5),
            Instr::jump(-7),
        ]);
        assert_eq!(annotate(&bc, 0), "  ; target=2");
        assert_eq!(annotate(&bc, 1), "  ; target=4");
        assert_eq!(annotate(&bc, 2), "  ; target=0");
        assert_eq!(annotate(&bc, 3), "  ; target=0");
        assert_eq!(annotate(&bc, 4), "  ; target=0");
        assert_eq!(annotate(&bc, 5), "  ; target=<out of range>");
    }

    #[test]
    fn listing_quotes_metadata_and_nests_children() {
        let program = crate::Program::compile(
            "local t = {}\nfunction f(a, ...) return function() return a end end\nprint(answer)",
            Some(b"evil\nfunction main".as_slice()),
        )
        .expect("source should compile");
        let text = program.disassemble().to_string();
        assert!(text.starts_with("function main name=- source=\"evil\\nfunction main\""));
        assert!(text.contains("\nfunction main/0 "));
        assert!(text.contains("\nfunction main/0/0 "));
        assert!(text.contains("; \"answer\", cache slot 1"));
        assert!(!text.contains("\nfunction main\n"));
        for line in text.lines() {
            assert!(
                line.is_empty() || line.starts_with("function main") || line.starts_with("  "),
                "malformed listing line: {line:?}"
            );
        }
    }
}
