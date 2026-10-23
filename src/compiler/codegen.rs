//! Register code generation from one function's syntax tree.
//!
//! Locals occupy the low registers in declaration order. Temporaries are
//! allocated last-in-first-out at `freereg`, and every call, concatenation
//! and constructor batch works on a fresh consecutive block at `freereg`, so
//! everything above such a block is dead while it runs; the VM relies on that
//! when it truncates the stack for a call.
//!
//! An expression is generated into a [`Val`] descriptor that may still be a
//! constant, a local's own register, or an instruction whose destination is
//! patched once its use is known. A local operand stays in its own register
//! only when nothing evaluated before its use can write the local: a local
//! that some closure captures is copied before a later sibling that may run
//! code, and the place operands of a multiple assignment are always copied.
//! Calls, concatenations and constructors copy their operands in order.

use super::Bytecode;
use super::ast::{
    Block, CallSuffix, Expr, ExprKind, FunctionBody, LocalDecl, LocalId, MethodSuffix, Place, Stmt,
    Suffix, TableAlloc, TableExpr, TableItem,
};
use crate::error::ParserFault;
use crate::instr::{
    ArgCount, ArithOp, ArithOperands, CompareOp, Instr, MAX_JUMP_OFFSET, OPEN_COUNT, RetCount,
};

/// Why code generation failed.
#[derive(Debug)]
pub(super) enum CodegenError {
    /// A jump spans `2^23` instructions or more.
    JumpTooFar,
    /// A function needs `count` registers, more than an operand can name.
    /// `line` is the best source line hint tracked during generation.
    TooManyRegisters { count: usize, line: u32 },
    /// Boxed so the variants stay within `variant_size_differences`.
    Internal(Box<ParserFault>),
}

type Result<T> = std::result::Result<T, CodegenError>;

// `reserve` admits frame tops below `OPEN_COUNT`; the published limit that
// `TooManyRegisters` reports must name the same bound.
const _: () = assert!(crate::limits::MAX_REGISTERS == OPEN_COUNT as usize - 1);

/// Generate `body`'s code into `chunk`.
pub(super) fn generate(chunk: &mut Bytecode, body: &FunctionBody) -> Result<()> {
    let mut generator = Generator {
        code: Vec::new(),
        lines: Vec::new(),
        line: 0,
        error_line: 0,
        freereg: u16::from(body.num_params),
        nactive: u16::from(body.num_params),
        max_stack: u16::from(body.num_params),
        locals: &body.locals,
        loops: Vec::new(),
    };
    generator.stmts(&body.body)?;
    generator.line = body.end_line;
    generator.emit(Instr::ret(0, RetCount::Fixed(0)));
    // An instruction emitted before any stamped node takes the next stamp.
    let mut next = body.end_line;
    for line in generator.lines.iter_mut().rev() {
        if *line == 0 {
            *line = next;
        } else {
            next = *line;
        }
    }
    chunk.code = generator.code;
    chunk.line_info = generator.lines;
    chunk.max_stack = generator.max_stack;
    Ok(())
}

/// An expression descriptor.
#[derive(Clone, Copy, Debug)]
enum Val {
    Nil,
    True,
    False,
    Num(u16),
    Str(u16),
    /// A local, read in its own register.
    Local {
        reg: u16,
        id: LocalId,
    },
    /// A value in a temporary register.
    Reg(u16),
    /// The instruction at this index produces the value into its A operand,
    /// not yet set.
    Reloc(usize),
    /// The call at this index; its results start at its base register.
    Call(usize),
    /// The vararg at this index; its values start at its A register.
    Vararg(usize),
}

struct LoopGen {
    breaks: Vec<usize>,
    break_close: Option<u8>,
}

struct Generator<'f> {
    code: Vec<Instr>,
    lines: Vec<u32>,
    /// The stamp for the next emitted instruction.
    line: u32,
    /// Best source line for an allocation failure, kept separate from
    /// instruction stamps so diagnostics do not change bytecode line info.
    error_line: u32,
    freereg: u16,
    nactive: u16,
    max_stack: u16,
    locals: &'f [LocalDecl],
    loops: Vec<LoopGen>,
}

impl Generator<'_> {
    fn emit(&mut self, instr: Instr) -> usize {
        self.code.push(instr);
        self.lines.push(self.line);
        self.code.len() - 1
    }

    fn emit_at(&mut self, instr: Instr, line: u32) -> usize {
        self.line = line;
        self.emit(instr)
    }

    fn internal(fault: ParserFault) -> CodegenError {
        CodegenError::Internal(Box::new(fault))
    }

    fn error_source_line(&self) -> u32 {
        if self.error_line == 0 {
            self.line
        } else {
            self.error_line
        }
    }

    // Registers

    fn reserve(&mut self, n: u16) -> Result<u16> {
        let first = self.freereg;
        let count = usize::from(first) + usize::from(n);
        self.freereg = match u16::try_from(count) {
            Ok(top) if top < OPEN_COUNT => top,
            _ => {
                return Err(CodegenError::TooManyRegisters {
                    count,
                    line: self.error_source_line(),
                });
            }
        };
        self.max_stack = self.max_stack.max(self.freereg);
        Ok(first)
    }

    fn free_reg(&mut self, reg: u16) {
        if reg >= self.nactive {
            debug_assert_eq!(reg + 1, self.freereg, "temporaries are freed in order");
            self.freereg = reg;
        }
    }

    fn free_val(&mut self, val: Val) {
        if let Val::Reg(reg) = val {
            self.free_reg(reg);
        }
    }

    /// Free two operand registers, the higher first.
    fn free_two(&mut self, a: Val, b: Val) {
        let ra = if let Val::Reg(r) = a { Some(r) } else { None };
        let rb = if let Val::Reg(r) = b { Some(r) } else { None };
        match (ra, rb) {
            (Some(x), Some(y)) if x > y => {
                self.free_reg(x);
                self.free_reg(y);
            }
            (Some(x), Some(y)) => {
                self.free_reg(y);
                self.free_reg(x);
            }
            (Some(x), None) => self.free_reg(x),
            (None, Some(y)) => self.free_reg(y),
            (None, None) => {}
        }
    }

    fn captured(&self, id: LocalId) -> bool {
        self.locals
            .get(id as usize)
            .is_some_and(|decl| decl.captured)
    }

    // Descriptors

    /// Settle a call or vararg to its single value.
    fn discharge(&mut self, val: Val) -> Val {
        match val {
            Val::Call(pc) => {
                self.code[pc] = self.code[pc].with_c(RetCount::Fixed(1).to_operand());
                Val::Reg(self.code[pc].a())
            }
            Val::Vararg(pc) => {
                self.code[pc] = self.code[pc].with_b(RetCount::Fixed(1).to_operand());
                Val::Reg(self.code[pc].a())
            }
            other => other,
        }
    }

    /// Put a discharged value into register `dst`.
    fn discharge_into(&mut self, val: Val, dst: u16) {
        match val {
            Val::Nil => {
                self.emit(Instr::load_nil(dst, 1));
            }
            Val::True => {
                self.emit(Instr::load_bool(dst, true));
            }
            Val::False => {
                self.emit(Instr::load_bool(dst, false));
            }
            Val::Num(idx) => {
                self.emit(Instr::load_num(dst, idx));
            }
            Val::Str(idx) => {
                self.emit(Instr::load_str(dst, idx));
            }
            Val::Local { reg, .. } | Val::Reg(reg) => {
                if reg != dst {
                    self.emit(Instr::mov(dst, reg));
                }
            }
            Val::Reloc(pc) => self.code[pc] = self.code[pc].with_a(dst),
            Val::Call(_) | Val::Vararg(_) => {
                let settled = self.discharge(val);
                self.discharge_into(settled, dst);
            }
        }
    }

    /// Put a value into a fresh register at the top.
    fn next_reg(&mut self, val: Val) -> Result<u16> {
        let val = self.discharge(val);
        self.free_val(val);
        let reg = self.reserve(1)?;
        self.discharge_into(val, reg);
        Ok(reg)
    }

    /// A register holding the value: a local's own register, or a temporary.
    fn any_reg(&mut self, val: Val) -> Result<u16> {
        match self.discharge(val) {
            Val::Local { reg, .. } | Val::Reg(reg) => Ok(reg),
            other => self.next_reg(other),
        }
    }

    /// An operand that is read only after code that `later_may_execute`
    /// says may run script or host code: a captured local is copied first,
    /// since that code may write it. Other locals stay in place; anything
    /// else settles into a register before the later code.
    fn operand_before(&mut self, val: Val, later_may_execute: bool) -> Result<Val> {
        match val {
            Val::Local { id, .. } if later_may_execute && self.captured(id) => {
                Ok(Val::Reg(self.next_reg(val)?))
            }
            Val::Local { .. } | Val::Nil | Val::True | Val::False | Val::Num(_) | Val::Str(_) => {
                Ok(val)
            }
            other => Ok(Val::Reg(self.any_reg(other)?)),
        }
    }

    /// The register of a prepared operand.
    fn operand_reg(&mut self, val: Val) -> Result<(u16, Val)> {
        match val {
            Val::Local { reg, .. } | Val::Reg(reg) => Ok((reg, val)),
            other => {
                let reg = self.next_reg(other)?;
                Ok((reg, Val::Reg(reg)))
            }
        }
    }

    /// Ask a call or vararg for `want` results starting at its base.
    fn set_returns(&mut self, val: Val, want: RetCount) -> Result<()> {
        let base = match val {
            Val::Call(pc) => {
                self.code[pc] = self.code[pc].with_c(want.to_operand());
                self.code[pc].a()
            }
            Val::Vararg(pc) => {
                self.code[pc] = self.code[pc].with_b(want.to_operand());
                self.code[pc].a()
            }
            _ => {
                return Err(Self::internal(
                    ParserFault::MultiValueTailMissingInstruction,
                ));
            }
        };
        self.freereg = base;
        if let RetCount::Fixed(n) = want {
            self.reserve(u16::from(n))?;
        }
        Ok(())
    }

    // Jumps

    fn jump(&mut self) -> usize {
        self.emit(Instr::jump(0))
    }

    fn patch(&mut self, pc: usize, target: usize) -> Result<()> {
        let offset = target as i64 - (pc as i64 + 1);
        if offset > i64::from(MAX_JUMP_OFFSET) || offset < -i64::from(MAX_JUMP_OFFSET) - 1 {
            return Err(CodegenError::JumpTooFar);
        }
        self.code[pc] = self.code[pc].with_offset(offset as i32);
        Ok(())
    }

    fn patch_here(&mut self, pcs: &[usize]) -> Result<()> {
        let here = self.code.len();
        for &pc in pcs {
            self.patch(pc, here)?;
        }
        Ok(())
    }

    // Statements

    fn stmts(&mut self, stmts: &[Stmt]) -> Result<()> {
        for stmt in stmts {
            self.stmt(stmt)?;
            debug_assert_eq!(
                self.freereg, self.nactive,
                "a statement frees its temporaries"
            );
        }
        Ok(())
    }

    /// A block in its own scope.
    fn block(&mut self, block: &Block) -> Result<()> {
        let saved = self.nactive;
        self.stmts(&block.stmts)?;
        self.close_scope(block.close, saved);
        Ok(())
    }

    /// Leave a scope: close its upvalues when the parser decided so, and
    /// drop its locals.
    fn close_scope(&mut self, close: Option<u8>, saved: u16) {
        if let Some(slot) = close {
            self.emit(Instr::close(u16::from(slot)));
        }
        self.nactive = saved;
        self.freereg = saved;
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match stmt {
            Stmt::Local {
                first,
                count,
                values,
            } => {
                debug_assert_eq!(self.freereg, u16::from(*first));
                let count = *count;
                if values.is_empty() {
                    let reg = self.reserve(count as u16)?;
                    self.emit(Instr::load_nil(reg, count as u16));
                } else {
                    self.explist_to_regs(values, count)?;
                }
                self.nactive = self.freereg;
            }
            Stmt::LocalFunction { reg, nested, line } => {
                self.error_line = *line;
                let reg = u16::from(*reg);
                debug_assert_eq!(self.freereg, reg);
                self.reserve(1)?;
                self.nactive = self.freereg;
                self.emit_at(Instr::closure(reg, u16::from(*nested)), *line);
            }
            Stmt::Assign {
                targets,
                values,
                line,
            } => {
                if let ([target], [value]) = (targets.as_slice(), values.as_slice()) {
                    self.single_assign(target, value, *line)?;
                } else {
                    self.multi_assign(targets, values, *line)?;
                }
            }
            Stmt::Call(expr) => {
                let val = self.expr(expr)?;
                let Val::Call(pc) = val else {
                    return Err(Self::internal(
                        ParserFault::MultiValueTailMissingInstruction,
                    ));
                };
                self.code[pc] = self.code[pc].with_c(RetCount::Fixed(0).to_operand());
                self.freereg = self.code[pc].a();
            }
            Stmt::Do(block) => self.block(block)?,
            Stmt::While {
                cond,
                body,
                body_close,
                break_close,
                scope_close,
            } => {
                let saved = self.nactive;
                let start = self.code.len();
                let exits = self.cond_false(cond)?;
                self.loops.push(LoopGen {
                    breaks: Vec::new(),
                    break_close: *break_close,
                });
                self.stmts(body)?;
                if let Some(slot) = body_close {
                    self.emit(Instr::close(u16::from(*slot)));
                }
                let back = self.jump();
                self.patch(back, start)?;
                self.patch_here(&exits)?;
                self.finish_loop()?;
                self.close_scope(*scope_close, saved);
            }
            Stmt::Repeat {
                body,
                cond,
                body_close,
                break_close,
                scope_close,
            } => {
                let saved = self.nactive;
                let start = self.code.len();
                self.loops.push(LoopGen {
                    breaks: Vec::new(),
                    break_close: *break_close,
                });
                self.stmts(body)?;
                let back = if let Some(slot) = body_close {
                    // The body's locals close after the condition reads
                    // them and before the jump back.
                    let val = self.expr(cond)?;
                    let reg = self.any_reg(val)?;
                    self.free_reg(reg);
                    self.emit(Instr::close(u16::from(*slot)));
                    vec![self.emit(Instr::test_false(reg, 0))]
                } else {
                    self.cond_false(cond)?
                };
                for pc in back {
                    self.patch(pc, start)?;
                }
                self.finish_loop()?;
                self.close_scope(*scope_close, saved);
            }
            Stmt::If { arms, else_block } => self.if_stmt(arms, else_block.as_ref())?,
            Stmt::NumericFor {
                base,
                start,
                limit,
                step,
                body,
                prep_line,
                loop_line,
                body_close,
                break_close,
                scope_close,
                ..
            } => {
                self.error_line = *prep_line;
                let saved = self.nactive;
                let base = u16::from(*base);
                debug_assert_eq!(self.freereg, base);
                for control in [start, limit, step] {
                    let val = self.expr(control)?;
                    self.next_reg(val)?;
                }
                self.reserve(1)?;
                self.nactive = self.freereg;
                let prep = self.emit_at(Instr::for_prep(base, 0), *prep_line);
                self.loops.push(LoopGen {
                    breaks: Vec::new(),
                    break_close: *break_close,
                });
                self.stmts(body)?;
                if *body_close {
                    self.emit(Instr::close(base + 3));
                }
                let lp = self.emit_at(Instr::for_loop(base, 0), *loop_line);
                self.patch(lp, prep + 1)?;
                self.patch(prep, lp + 1)?;
                self.finish_loop()?;
                self.close_scope(*scope_close, saved);
            }
            Stmt::GenericFor {
                base,
                vars,
                exprs,
                body,
                call_line,
                body_close,
                break_close,
                scope_close,
            } => {
                self.error_line = *call_line;
                let saved = self.nactive;
                let base = u16::from(*base);
                debug_assert_eq!(self.freereg, base);
                self.explist_to_regs(exprs, 3)?;
                self.reserve(u16::from(*vars))?;
                self.nactive = self.freereg;
                let call = self.emit_at(Instr::tfor_call(base, *vars, None), *call_line);
                let test = self.emit_at(Instr::tfor_loop(base, 0), *call_line);
                self.loops.push(LoopGen {
                    breaks: Vec::new(),
                    break_close: *break_close,
                });
                self.stmts(body)?;
                if *body_close {
                    self.emit(Instr::close(base + 3));
                }
                let back = self.jump();
                self.patch(back, call)?;
                self.patch_here(&[test])?;
                self.finish_loop()?;
                self.close_scope(*scope_close, saved);
            }
            Stmt::Function {
                target,
                nested,
                line,
            } => self.function_stmt(target, *nested, *line)?,
            Stmt::Return { values, line } => self.return_stmt(values, *line)?,
            Stmt::Break => {
                let Some(close) = self.loops.last().map(|lp| lp.break_close) else {
                    return Err(Self::internal(ParserFault::BreakEmissionLostLoopContext));
                };
                if let Some(slot) = close {
                    self.emit(Instr::close(u16::from(slot)));
                }
                let pc = self.jump();
                if let Some(lp) = self.loops.last_mut() {
                    lp.breaks.push(pc);
                }
            }
        }
        Ok(())
    }

    /// Patch the innermost loop's breaks to here and leave it.
    fn finish_loop(&mut self) -> Result<()> {
        let lp = self
            .loops
            .pop()
            .ok_or_else(|| Self::internal(ParserFault::LoopExitWithoutEntry))?;
        self.patch_here(&lp.breaks)
    }

    fn if_stmt(&mut self, arms: &[super::ast::IfArm], else_block: Option<&Block>) -> Result<()> {
        let mut end_jumps = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let exits = self.cond_false(&arm.cond)?;
            self.block(&arm.block)?;
            let followed = index + 1 < arms.len() || else_block.is_some();
            // An arm whose own last statement is `return` cannot fall off
            // its end, so its jump to the end of the statement is dead.
            let returns = matches!(arm.block.stmts.last(), Some(Stmt::Return { .. }));
            if followed && !returns {
                end_jumps.push(self.jump());
            }
            self.patch_here(&exits)?;
        }
        if let Some(block) = else_block {
            self.block(block)?;
        }
        self.patch_here(&end_jumps)
    }

    /// Evaluate `values` into fresh consecutive registers, adjusted to
    /// exactly `want` values: an expandable final call or vararg supplies
    /// what the fixed values leave, missing values are nil, extra values are
    /// evaluated and dropped.
    fn explist_to_regs(&mut self, values: &[Expr], want: usize) -> Result<()> {
        let base = self.freereg;
        let count = usize::from(base).saturating_add(want);
        let want = u16::try_from(want).map_err(|_| CodegenError::TooManyRegisters {
            count,
            line: self.error_source_line(),
        })?;
        for (index, value) in values.iter().enumerate() {
            let val = self.expr(value)?;
            let last = index + 1 == values.len();
            if last && value.is_multi() && usize::from(want) > index {
                let needed = u8::try_from(usize::from(want) - index)
                    .map_err(|_| Self::internal(ParserFault::MultiValueTailMissingInstruction))?;
                self.set_returns(val, RetCount::Fixed(needed))?;
            } else {
                self.next_reg(val)?;
            }
        }
        let have = self.freereg - base;
        if have < want {
            let missing = want - have;
            let reg = self.reserve(missing)?;
            self.emit(Instr::load_nil(reg, missing));
        } else if have > want {
            self.freereg = base + want;
        }
        Ok(())
    }

    fn single_assign(&mut self, target: &Place, value: &Expr, line: u32) -> Result<()> {
        self.error_line = line;
        match target {
            Place::Local(reg) => {
                let val = self.expr(value)?;
                let val = self.discharge(val);
                self.free_val(val);
                self.discharge_into(val, u16::from(*reg));
            }
            Place::Upval(index) => {
                let val = self.expr(value)?;
                let reg = self.any_reg(val)?;
                self.emit_at(Instr::set_upval(reg, u16::from(*index)), line);
                self.free_reg(reg);
            }
            Place::Global(name) => {
                let val = self.expr(value)?;
                let reg = self.any_reg(val)?;
                self.emit_at(Instr::set_global(reg, *name, None), line);
                self.free_reg(reg);
            }
            Place::Builtin(builtin) => {
                let val = self.expr(value)?;
                let reg = self.any_reg(val)?;
                self.emit_at(Instr::set_builtin(reg, *builtin), line);
                self.free_reg(reg);
            }
            Place::Field { object, name } => {
                let obj = self.expr(object)?;
                let obj = self.operand_before(obj, value.may_execute)?;
                let (obj_reg, obj) = self.operand_reg(obj)?;
                let val = self.expr(value)?;
                let src = self.any_reg(val)?;
                self.emit_at(Instr::set_field(obj_reg, *name, src, None), line);
                self.free_reg(src);
                self.free_val(obj);
            }
            Place::Index { object, key } => {
                let obj = self.expr(object)?;
                let obj = self.operand_before(obj, key.may_execute || value.may_execute)?;
                let (obj_reg, obj) = self.operand_reg(obj)?;
                let k = self.expr(key)?;
                let k = self.operand_before(k, value.may_execute)?;
                let (key_reg, k) = self.operand_reg(k)?;
                let val = self.expr(value)?;
                let src = self.any_reg(val)?;
                self.emit_at(Instr::set_table(obj_reg, key_reg, src), line);
                self.free_reg(src);
                self.free_two(obj, k);
            }
        }
        Ok(())
    }

    /// `targets = values` with several targets or values. Every place operand
    /// is copied before the right-hand side runs and every value lands in a
    /// fresh register, so the stores, last target first, see the operands
    /// and values as they were evaluated.
    fn multi_assign(&mut self, targets: &[Place], values: &[Expr], line: u32) -> Result<()> {
        self.error_line = line;
        let operands_base = self.freereg;
        let mut place_regs = Vec::with_capacity(targets.len());
        for target in targets {
            place_regs.push(match target {
                Place::Field { object, .. } => {
                    let obj = self.expr(object)?;
                    (Some(self.next_reg(obj)?), None)
                }
                Place::Index { object, key } => {
                    let obj = self.expr(object)?;
                    let obj = self.next_reg(obj)?;
                    let k = self.expr(key)?;
                    (Some(obj), Some(self.next_reg(k)?))
                }
                _ => (None, None),
            });
        }
        let values_base = self.freereg;
        self.explist_to_regs(values, targets.len())?;
        for (index, (target, (obj, key))) in targets.iter().zip(&place_regs).enumerate().rev() {
            let count = usize::from(values_base).saturating_add(index);
            let src =
                u16::try_from(count).map_err(|_| CodegenError::TooManyRegisters { count, line })?;
            self.line = line;
            match target {
                Place::Local(reg) => {
                    if u16::from(*reg) != src {
                        self.emit(Instr::mov(u16::from(*reg), src));
                    }
                }
                Place::Upval(upvalue) => {
                    self.emit(Instr::set_upval(src, u16::from(*upvalue)));
                }
                Place::Global(name) => {
                    self.emit(Instr::set_global(src, *name, None));
                }
                Place::Builtin(builtin) => {
                    self.emit(Instr::set_builtin(src, *builtin));
                }
                Place::Field { name, .. } => {
                    let obj = obj.ok_or_else(|| {
                        Self::internal(ParserFault::AssignmentPlaceRegistersMissing)
                    })?;
                    self.emit(Instr::set_field(obj, *name, src, None));
                }
                Place::Index { .. } => {
                    let (Some(obj), Some(key)) = (*obj, *key) else {
                        return Err(Self::internal(ParserFault::AssignmentPlaceRegistersMissing));
                    };
                    self.emit(Instr::set_table(obj, key, src));
                }
            }
        }
        self.freereg = operands_base;
        Ok(())
    }

    fn function_stmt(&mut self, target: &Place, nested: u8, line: u32) -> Result<()> {
        self.error_line = line;
        match target {
            Place::Local(reg) => {
                self.emit_at(Instr::closure(u16::from(*reg), u16::from(nested)), line);
            }
            Place::Field { object, name } => {
                let obj = self.expr(object)?;
                let obj = self.any_reg(obj)?;
                let closure = self.reserve(1)?;
                self.emit_at(Instr::closure(closure, u16::from(nested)), line);
                self.emit_at(Instr::set_field(obj, *name, closure, None), line);
                self.free_reg(closure);
                self.free_reg(obj);
            }
            Place::Upval(_) | Place::Global(_) | Place::Builtin(_) => {
                let closure = self.reserve(1)?;
                self.emit_at(Instr::closure(closure, u16::from(nested)), line);
                match target {
                    Place::Upval(index) => {
                        self.emit(Instr::set_upval(closure, u16::from(*index)));
                    }
                    Place::Global(name) => {
                        self.emit(Instr::set_global(closure, *name, None));
                    }
                    Place::Builtin(builtin) => {
                        self.emit(Instr::set_builtin(closure, *builtin));
                    }
                    _ => {}
                }
                self.free_reg(closure);
            }
            Place::Index { .. } => {
                return Err(Self::internal(ParserFault::NamedFunctionTargetNotVariable));
            }
        }
        Ok(())
    }

    fn return_stmt(&mut self, values: &[Expr], line: u32) -> Result<()> {
        self.error_line = line;
        match values {
            [] => {
                self.emit_at(Instr::ret(0, RetCount::Fixed(0)), line);
            }
            [value] if !value.is_multi() => {
                let val = self.expr(value)?;
                let reg = self.any_reg(val)?;
                self.emit_at(Instr::ret(reg, RetCount::Fixed(1)), line);
                self.free_reg(reg);
            }
            _ => {
                let base = self.freereg;
                let (last, fixed) = values
                    .split_last()
                    .ok_or_else(|| Self::internal(ParserFault::EmptyReturnList))?;
                for value in fixed {
                    let val = self.expr(value)?;
                    self.next_reg(val)?;
                }
                let val = self.expr(last)?;
                let count = if last.is_multi() {
                    self.set_returns(val, RetCount::All)?;
                    RetCount::All
                } else {
                    self.next_reg(val)?;
                    let n = u8::try_from(values.len())
                        .map_err(|_| Self::internal(ParserFault::ReturnCountOutOfRange))?;
                    RetCount::Fixed(n)
                };
                self.emit_at(Instr::ret(base, count), line);
                self.freereg = base;
            }
        }
        Ok(())
    }

    // Conditions

    /// Generate a condition and return the jumps taken when it is false.
    fn cond_false(&mut self, cond: &Expr) -> Result<Vec<usize>> {
        match &cond.kind {
            ExprKind::True | ExprKind::Num(_) | ExprKind::Str(_) => Ok(Vec::new()),
            ExprKind::Nil | ExprKind::False => Ok(vec![self.jump()]),
            ExprKind::Compare(chain) => {
                let (last, init) = chain
                    .rest
                    .split_last()
                    .ok_or_else(|| Self::internal(ParserFault::EmptyComparisonChain))?;
                let mut acc = self.expr(&chain.first)?;
                for link in init {
                    acc = self.compare_value(link.op, acc, &link.rhs, link.line)?;
                }
                let lhs = self.operand_before(acc, last.rhs.may_execute)?;
                let (lhs_reg, lhs) = self.operand_reg(lhs)?;
                let rhs = self.expr(&last.rhs)?;
                let rhs_reg = self.any_reg(rhs)?;
                let pc = self.emit_at(
                    Instr::branch_compare(last.op, lhs_reg, rhs_reg, 0),
                    last.line,
                );
                self.free_two(lhs, Val::Reg(rhs_reg));
                Ok(vec![pc])
            }
            _ => {
                let val = self.expr(cond)?;
                let reg = self.any_reg(val)?;
                self.free_reg(reg);
                Ok(vec![self.emit(Instr::test_false(reg, 0))])
            }
        }
    }

    // Expressions

    fn expr(&mut self, expr: &Expr) -> Result<Val> {
        Ok(match &expr.kind {
            ExprKind::Nil => Val::Nil,
            ExprKind::True => Val::True,
            ExprKind::False => Val::False,
            ExprKind::Num(idx) => Val::Num(*idx),
            ExprKind::Str(idx) => Val::Str(*idx),
            ExprKind::Vararg { line } => {
                self.error_line = *line;
                let reg = self.reserve(1)?;
                Val::Vararg(self.emit_at(Instr::vararg(reg, RetCount::Fixed(1)), *line))
            }
            ExprKind::Local { reg, id } => Val::Local {
                reg: u16::from(*reg),
                id: *id,
            },
            ExprKind::Upval(index) => Val::Reloc(self.emit(Instr::get_upval(0, u16::from(*index)))),
            ExprKind::Global { name, line } => {
                Val::Reloc(self.emit_at(Instr::get_global(0, *name, None), *line))
            }
            ExprKind::Builtin(builtin) => Val::Reloc(self.emit(Instr::get_builtin(0, *builtin))),
            ExprKind::Function { nested, line } => {
                Val::Reloc(self.emit_at(Instr::closure(0, u16::from(*nested)), *line))
            }
            ExprKind::Table(table) => {
                self.error_line = table.line;
                self.table(table)?
            }
            ExprKind::Suffixed(suffixed) => {
                let mut val = self.expr(&suffixed.base)?;
                for suffix in &suffixed.suffixes {
                    val = self.suffix(val, suffix)?;
                }
                val
            }
            ExprKind::Paren(inner) => {
                let val = self.expr(inner)?;
                self.discharge(val)
            }
            ExprKind::Arith(chain) => {
                let mut acc = self.expr(&chain.first)?;
                for link in &chain.rest {
                    acc = self.arith(link.op, acc, &link.rhs, link.line)?;
                }
                acc
            }
            ExprKind::Pow {
                base,
                exponent,
                line,
            } => {
                let acc = self.expr(base)?;
                self.arith(ArithOp::Pow, acc, exponent, *line)?
            }
            ExprKind::Concat { operands, line } => {
                let first = self.freereg;
                for operand in operands {
                    let val = self.expr(operand)?;
                    self.next_reg(val)?;
                }
                self.freereg = first;
                Val::Reloc(self.emit_at(Instr::concat(0, first, operands.len() as u16), *line))
            }
            ExprKind::Compare(chain) => {
                let mut acc = self.expr(&chain.first)?;
                for link in &chain.rest {
                    acc = self.compare_value(link.op, acc, &link.rhs, link.line)?;
                }
                acc
            }
            ExprKind::And(operands) => self.logical(operands, false)?,
            ExprKind::Or(operands) => self.logical(operands, true)?,
            ExprKind::Not { operand, line } => {
                let val = self.expr(operand)?;
                self.error_line = *line;
                let reg = self.any_reg(val)?;
                self.free_reg(reg);
                Val::Reloc(self.emit_at(Instr::not(0, reg), *line))
            }
            ExprKind::Len { operand, line } => {
                let val = self.expr(operand)?;
                self.error_line = *line;
                let reg = self.any_reg(val)?;
                self.free_reg(reg);
                Val::Reloc(self.emit_at(Instr::len(0, reg), *line))
            }
            ExprKind::Neg { operand, line } => {
                let val = self.expr(operand)?;
                self.error_line = *line;
                let reg = self.any_reg(val)?;
                self.free_reg(reg);
                Val::Reloc(self.emit_at(Instr::unm(0, reg), *line))
            }
        })
    }

    /// `left op right` for an arithmetic operator.
    fn arith(&mut self, op: ArithOp, left: Val, right: &Expr, line: u32) -> Result<Val> {
        self.error_line = line;
        let left = self.operand_before(left, right.may_execute)?;
        let rhs = self.expr(right)?;
        let rhs = match self.discharge(rhs) {
            Val::Num(idx) => Val::Num(idx),
            other => Val::Reg(self.any_reg(other)?),
        };
        let (operands, b, c, left) = match (left, rhs) {
            (Val::Num(lhs), Val::Num(rhs_idx)) => {
                // A fold the parser refused (NaN, negative zero, full pool):
                // the operation stays charged at runtime.
                let reg = self.next_reg(Val::Num(lhs))?;
                (ArithOperands::RegConst, reg, rhs_idx, Val::Reg(reg))
            }
            (Val::Num(lhs), Val::Reg(reg)) => (ArithOperands::ConstReg, lhs, reg, Val::Num(lhs)),
            (left, Val::Num(rhs_idx)) => {
                let (reg, left) = self.operand_reg(left)?;
                (ArithOperands::RegConst, reg, rhs_idx, left)
            }
            (left, rhs) => {
                let (lhs_reg, left) = self.operand_reg(left)?;
                let Val::Reg(rhs_reg) = rhs else {
                    return Err(Self::internal(ParserFault::FoldedLiteralOutOfRange));
                };
                (ArithOperands::RegReg, lhs_reg, rhs_reg, left)
            }
        };
        let rhs_val = match operands {
            ArithOperands::RegConst => Val::Num(c),
            _ => Val::Reg(c),
        };
        self.free_two(left, rhs_val);
        Ok(Val::Reloc(
            self.emit_at(Instr::arith(op, operands, 0, b, c), line),
        ))
    }

    /// `left op right` for a comparison producing a boolean.
    fn compare_value(&mut self, op: CompareOp, left: Val, right: &Expr, line: u32) -> Result<Val> {
        self.error_line = line;
        let left = self.operand_before(left, right.may_execute)?;
        let (lhs_reg, left) = self.operand_reg(left)?;
        let rhs = self.expr(right)?;
        let rhs_reg = self.any_reg(rhs)?;
        self.free_two(left, Val::Reg(rhs_reg));
        Ok(Val::Reloc(
            self.emit_at(Instr::compare(op, 0, lhs_reg, rhs_reg), line),
        ))
    }

    /// `a and b and ...` (or `or` when `or` is set): each operand lands in
    /// one fresh register, and a falsy (truthy) one skips the rest.
    fn logical(&mut self, operands: &[Expr], or: bool) -> Result<Val> {
        let dst = self.reserve(1)?;
        let mut exits = Vec::new();
        for (index, operand) in operands.iter().enumerate() {
            let val = self.expr(operand)?;
            let val = self.discharge(val);
            self.free_val(val);
            self.discharge_into(val, dst);
            if index + 1 < operands.len() {
                exits.push(self.emit(if or {
                    Instr::test_true(dst, 0)
                } else {
                    Instr::test_false(dst, 0)
                }));
            }
        }
        self.patch_here(&exits)?;
        Ok(Val::Reg(dst))
    }

    fn suffix(&mut self, object: Val, suffix: &Suffix) -> Result<Val> {
        match suffix {
            Suffix::Field { name, line } => {
                let reg = self.any_reg(object)?;
                self.free_reg(reg);
                Ok(Val::Reloc(
                    self.emit_at(Instr::get_field(0, reg, *name, None), *line),
                ))
            }
            Suffix::Index { key, line } => {
                let obj = self.operand_before(object, key.may_execute)?;
                let (obj_reg, obj) = self.operand_reg(obj)?;
                let k = self.expr(key)?;
                let key_reg = self.any_reg(k)?;
                self.free_two(obj, Val::Reg(key_reg));
                Ok(Val::Reloc(
                    self.emit_at(Instr::get_table(0, obj_reg, key_reg), *line),
                ))
            }
            Suffix::Call(call) => self.call(object, call),
            Suffix::Method(method) => self.method_call(object, method),
        }
    }

    /// Evaluate call arguments into the registers after the callee and
    /// return the argument count.
    fn call_args(&mut self, args: &[Expr], implicit: u8) -> Result<ArgCount> {
        let Some((last, fixed)) = args.split_last() else {
            return Ok(ArgCount::Fixed(implicit));
        };
        for arg in fixed {
            let val = self.expr(arg)?;
            self.next_reg(val)?;
        }
        let val = self.expr(last)?;
        if last.is_multi() {
            self.set_returns(val, RetCount::All)?;
            Ok(ArgCount::Dynamic)
        } else {
            self.next_reg(val)?;
            let count = u8::try_from(args.len() + usize::from(implicit))
                .map_err(|_| Self::internal(ParserFault::MultiValueTailMissingInstruction))?;
            Ok(ArgCount::Fixed(count))
        }
    }

    fn call(&mut self, callee: Val, call: &CallSuffix) -> Result<Val> {
        self.error_line = call.line;
        let base = self.next_reg(callee)?;
        let args = self.call_args(&call.args, 0)?;
        let pc = self.emit_at(
            Instr::call(base, args, RetCount::Fixed(1), false),
            call.line,
        );
        self.freereg = base;
        self.reserve(1)?;
        Ok(Val::Call(pc))
    }

    fn method_call(&mut self, object: Val, method: &MethodSuffix) -> Result<Val> {
        self.error_line = method.lookup_line;
        let obj = self.any_reg(object)?;
        self.free_reg(obj);
        let base = self.reserve(2)?;
        self.emit_at(
            Instr::self_(base, obj, method.name, None),
            method.lookup_line,
        );
        let args = self.call_args(&method.args, 1)?;
        let pc = self.emit_at(
            Instr::call(base, args, RetCount::Fixed(1), true),
            method.line,
        );
        self.freereg = base;
        self.reserve(1)?;
        Ok(Val::Call(pc))
    }

    fn table(&mut self, table: &TableExpr) -> Result<Val> {
        let reg = self.reserve(1)?;
        let alloc = match table.alloc {
            TableAlloc::Plain => Instr::new_table(reg),
            TableAlloc::Presized(capacity) => Instr::new_table_presized(reg, capacity),
            TableAlloc::Template(template) => Instr::new_table_template(reg, template),
        };
        self.emit_at(alloc, table.line);
        for (index, item) in table.items.iter().enumerate() {
            match item {
                TableItem::Positional(value) => {
                    let val = self.expr(value)?;
                    let open = matches!(
                        table.items.get(index + 1),
                        Some(TableItem::Flush { count: None, .. })
                    ) && value.is_multi();
                    if open {
                        self.set_returns(val, RetCount::All)?;
                    } else {
                        self.next_reg(val)?;
                    }
                }
                TableItem::Named {
                    name,
                    value,
                    line,
                    pinned,
                } => {
                    let val = self.expr(value)?;
                    let src = self.any_reg(val)?;
                    let instr = match pinned {
                        Some(entry) => Instr::init_field_pinned(reg, *name, src, *entry),
                        None => Instr::init_field(reg, *name, src),
                    };
                    self.emit_at(instr, *line);
                    self.free_reg(src);
                }
                TableItem::Keyed { key, value, line } => {
                    let k = self.expr(key)?;
                    let k = self.operand_before(k, value.may_execute)?;
                    let (key_reg, k) = self.operand_reg(k)?;
                    let val = self.expr(value)?;
                    let src = self.any_reg(val)?;
                    self.emit_at(Instr::init_index(reg, key_reg, src), *line);
                    self.free_two(k, Val::Reg(src));
                }
                TableItem::Flush { count, batch, line } => {
                    self.emit_at(Instr::set_list(reg, *count, *batch), *line);
                    self.freereg = reg + 1;
                }
            }
        }
        Ok(Val::Reg(reg))
    }
}

#[cfg(test)]
mod tests {
    use super::{CodegenError, Generator, MAX_JUMP_OFFSET};

    #[test]
    fn register_exhaustion_is_a_limit_and_preserves_the_allocator() {
        let mut generator = Generator {
            code: Vec::new(),
            lines: Vec::new(),
            line: 1,
            error_line: 2,
            freereg: 0,
            nactive: 0,
            max_stack: 0,
            locals: &[],
            loops: Vec::new(),
        };
        generator.reserve(u16::MAX - 1).expect("largest frame fits");
        for extra in [1, 2] {
            assert!(matches!(
                generator.reserve(extra),
                Err(CodegenError::TooManyRegisters { count, line: 2 })
                    if count == usize::from(u16::MAX - 1) + usize::from(extra)
            ));
            assert_eq!(generator.freereg, u16::MAX - 1);
        }
    }

    /// A jump reaches exactly `MAX_JUMP_OFFSET` instructions forward; one
    /// further fails compilation rather than wrapping. No source this test
    /// could compile reaches that far without gigabytes of tree, so the
    /// patch is driven directly.
    #[test]
    fn jump_patching_rejects_offsets_past_the_encoding() {
        let mut generator = Generator {
            code: Vec::new(),
            lines: Vec::new(),
            line: 1,
            error_line: 0,
            freereg: 0,
            nactive: 0,
            max_stack: 0,
            locals: &[],
            loops: Vec::new(),
        };
        let pc = generator.jump();
        let farthest = usize::try_from(MAX_JUMP_OFFSET).expect("the offset is positive") + 1;
        generator
            .patch(pc, farthest)
            .expect("the farthest encodable target patches");
        assert_eq!(generator.code[pc].sj(), MAX_JUMP_OFFSET);
        assert!(matches!(
            generator.patch(pc, farthest + 1),
            Err(CodegenError::JumpTooFar)
        ));
    }
}
