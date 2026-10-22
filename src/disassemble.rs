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
use crate::instr::{Instr, RetCount};

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
        Some(source) => write_quoted(f, source.as_bytes())?,
        None => f.write_str("-")?,
    }
    writeln!(
        f,
        " params={} vararg={} local_slots={}",
        bc.num_params, bc.is_vararg, bc.num_locals
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
        None => f.write_str("<out of range>"),
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

    // Literal-pool operands in Bx (the verifier's set of string-literal ops).
    match op {
        Instr::OP_PUSH_NUM => {
            notes.next(f)?;
            match bc.number_literals.get(usize::from(inst.bx())) {
                Some(n) => write!(f, "{n:?}")?,
                None => f.write_str("<out of range>")?,
            }
        }
        Instr::OP_PUSH_STRING
        | Instr::OP_GET_GLOBAL
        | Instr::OP_SET_GLOBAL
        | Instr::OP_GET_FIELD
        | Instr::OP_SET_FIELD
        | Instr::OP_INIT_FIELD
        | Instr::OP_INIT_FIELD_PINNED
        | Instr::OP_SET_FIELD_AT => {
            notes.next(f)?;
            write_string_ref(f, bc, inst.bx())?;
        }
        Instr::OP_NEW_TABLE_TEMPLATE => {
            notes.next(f)?;
            write!(f, "template {}", inst.a())?;
        }
        Instr::OP_CLOSURE => {
            notes.next(f)?;
            if usize::from(inst.a()) < bc.nested.len() {
                write!(f, "function {path}/{}", inst.a())?;
            } else {
                f.write_str("function <out of range>")?;
            }
        }
        _ => {}
    }

    // Cache operands, decoded by `Instr`'s cache-slot decoders.
    let cache = match op {
        Instr::OP_GET_GLOBAL | Instr::OP_SET_GLOBAL => Some(inst.global_cache_slot()),
        Instr::OP_GET_FIELD => Some(inst.field_cache_slot()),
        Instr::OP_SET_FIELD => Some(inst.set_field_cache_slot()),
        Instr::OP_TFOR_CALL => Some(inst.tfor_cursor_slot()),
        _ => None,
    };
    if let Some(cache) = cache {
        notes.next(f)?;
        match cache {
            Some(slot) => write!(f, "cache slot {slot}")?,
            None => f.write_str("uncached")?,
        }
    }

    match op {
        Instr::OP_VARARG if inst.vararg_count() == RetCount::All => {
            notes.next(f)?;
            f.write_str("all values")?;
        }
        Instr::OP_SET_LIST if inst.a() == 0 => {
            notes.next(f)?;
            f.write_str("dynamic count")?;
        }
        _ => {}
    }

    if Instr::is_transfer(op) {
        notes.next(f)?;
        let offset = inst.sbx();
        let target = pc.checked_add(1).and_then(|next| {
            if offset >= 0 {
                next.checked_add(offset as usize)
            } else {
                next.checked_sub(offset.unsigned_abs() as usize)
            }
        });
        match target {
            Some(target) if target < bc.code.len() => write!(f, "target={target}")?,
            _ => f.write_str("target=<out of range>")?,
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
            Instr::get_global(0, None),
            Instr::get_global(0, Some(254)),
            Instr::set_global(0, None),
            Instr::set_global(0, Some(254)),
            Instr::get_field(0, None),
            Instr::get_field(0, Some(3)),
            Instr::set_field(0, None),
            Instr::tfor_call(0, 1, None),
            Instr::tfor_call(0, 1, Some(254)),
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
    }

    #[test]
    fn count_sentinels_pools_and_children_are_named() {
        let bc = with_code(vec![
            Instr::vararg(RetCount::All),
            Instr::vararg(RetCount::Fixed(2)),
            Instr::set_list_batch(0, 1),
            Instr::set_list_batch(3, 0),
            Instr::push_num(0),
            Instr::push_num(9),
            Instr::closure(0),
            Instr::closure(1),
            Instr::new_table_template(0),
            Instr::ret(RetCount::Fixed(0)),
        ]);
        assert_eq!(annotate(&bc, 0), "  ; all values");
        assert_eq!(annotate(&bc, 1), "");
        assert_eq!(annotate(&bc, 2), "  ; dynamic count");
        assert_eq!(annotate(&bc, 3), "");
        assert_eq!(annotate(&bc, 4), "  ; -0.0");
        assert_eq!(annotate(&bc, 5), "  ; <out of range>");
        assert_eq!(annotate(&bc, 6), "  ; function main/0");
        assert_eq!(annotate(&bc, 7), "  ; function <out of range>");
        assert_eq!(annotate(&bc, 8), "  ; template 0");
        assert_eq!(annotate(&bc, 9), "");
    }

    #[test]
    fn transfer_targets_are_pc_plus_one_plus_offset() {
        let bc = with_code(vec![
            Instr::jump(1),
            Instr::for_prep(0, 2),
            Instr::branch_false_less(-3),
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
        let program = crate::Engine::new()
            .compile_named(
                "local t = {}\nfunction f(a, ...) return function() return a end end\nprint(answer)",
                "evil\nfunction main",
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
