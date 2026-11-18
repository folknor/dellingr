//! Numeric `for` loops whose bodies are straight-line number arithmetic run
//! as one typed region: the registers the loop touches live in an `f64`
//! mirror, and each iteration runs its body operations and the loop step
//! without going back through the dispatch loop.
//!
//! The region is derived metadata, keyed by the canonical `FOR_PREP` pc and
//! built per State from admitted bytecode (fresh compiles and snapshot loads
//! alike), so nothing here is persisted or admitted by the verifier. The
//! canonical instructions stay in place and remain the fallback: a loop the
//! entry guard refuses, and any jump into the body from elsewhere, runs them.
//!
//! Why the region is exact, observable boundary by observable boundary:
//!
//! - Types. A body holds only arithmetic, `UNM`, `MOVE` and `LOAD_NUM`, so
//!   numbers in give numbers out, and nothing else runs while the region
//!   does: no call, allocation, upvalue access or handler. The only foreign
//!   code is the back-edge checkpoint (`State::poll_gc_checkpoint`), which
//!   polls the interrupt and may collect, and runs no Lua or host code. A
//!   guard at entry on every register read before it is written in one pass
//!   of the body and the loop step therefore proves every read of the whole
//!   loop is a number. The guard reads operands before the destination is
//!   marked written, so `R0 = R0 + k` guards `R0`.
//! - Registers. The stack is brought level with the mirror before the first
//!   back-edge checkpoint and on every exit (loop end, refused charge,
//!   interrupt). Only slots written since entry are published, so a slot
//!   whose first write has not happened yet keeps the value canonical
//!   execution would have left in it. After the first checkpoint every
//!   written slot holds a number on the stack; stale numbers root nothing, so
//!   later checkpoints see the roots canonical execution would, and `%p`
//!   identities and collection timing are unchanged.
//! - Charges. Each iteration's fixed weights, body operations then
//!   `FOR_LOOP`, are added in one step when their sum stays below the flush
//!   threshold, which leaves the pending batch exactly where per-operation
//!   charging would. Otherwise the iteration charges operation by operation,
//!   as `add_cost!` does, with the cursor naming each operation before its
//!   charge, so a refusal names the same instruction and leaves the same
//!   completed prefix. Zero weights are skipped as dispatch skips them.
//! - The loop step reads the mirror's controls, which a body may have
//!   written, computes the next value into a temporary, polls on the taken
//!   back edge with the cursor past `FOR_LOOP`, and only then writes the
//!   hidden and visible counters, so an interrupted iteration writes no
//!   counter.

use super::Result;
use super::State;
use super::Val;
use super::frame::Frame;
use crate::compiler::Bytecode;
use crate::cost_meter::CostState;
use crate::instr::{ArithOp, ArithOperands, Instr};
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

/// Mirror slots per region: registers first, then number literals. A loop
/// touching more is left to canonical dispatch.
const MIRROR_SLOTS: usize = 32;

/// The numeric regions of one chunk, sorted by their `FOR_PREP` pc.
#[derive(Debug, Default)]
pub(super) struct NumericLoops {
    loops: Box<[NumericLoop]>,
}

#[derive(Debug)]
pub(super) struct NumericLoop {
    /// The `FOR_PREP` pc.
    prep: usize,
    /// Mirror slot -> frame register, for the register slots. Slots 0 to 3
    /// are the loop controls (start/counter, limit, step, visible variable).
    registers: Box<[u16]>,
    /// Register slots loaded and checked at entry: read before written.
    guards: u32,
    /// Literal mirror slots and their values, loaded at entry.
    literals: Box<[(u8, f64)]>,
    ops: Box<[LoopOp]>,
    /// Each body operation's fixed weight at this State's model.
    weights: Box<[u64]>,
    /// `FOR_LOOP`'s fixed weight.
    step_weight: u64,
    /// Sum of `weights` and `step_weight`, saturating.
    iteration_weight: u64,
}

#[derive(Clone, Copy, Debug)]
struct LoopOp {
    kind: OpKind,
    /// Destination mirror slot (always a register slot).
    a: u8,
    b: u8,
    c: u8,
}

#[derive(Clone, Copy, Debug)]
enum OpKind {
    Arith(ArithOp),
    Unm,
    /// `MOVE` and `LOAD_NUM` (from a literal slot).
    Copy,
}

impl NumericLoops {
    /// Find every numeric loop of `bytecode` that qualifies as a region,
    /// with fixed weights from `cost`, which a State fixes when it is built.
    pub(super) fn new(bytecode: &Bytecode, cost: &CostState) -> Self {
        let code = &bytecode.code;
        let mut loops = Vec::new();
        for (prep, inst) in code.iter().enumerate() {
            if inst.opcode() == Instr::OP_FOR_PREP
                && let Some(region) = NumericLoop::recognize(bytecode, prep, cost)
            {
                loops.push(region);
            }
        }
        Self {
            loops: loops.into_boxed_slice(),
        }
    }

    #[inline]
    pub(super) fn get(&self, prep: usize) -> Option<&NumericLoop> {
        if self.loops.is_empty() {
            return None;
        }
        match self.loops.binary_search_by_key(&prep, |region| region.prep) {
            Ok(index) => Some(&self.loops[index]),
            Err(_) => None,
        }
    }
}

/// Builds a region's mirror layout.
struct Layout {
    registers: Vec<u16>,
    literals: Vec<(u8, f64)>,
    literal_ids: Vec<u16>,
    written: u32,
    guards: u32,
}

impl Layout {
    fn register(&mut self, register: u16) -> Option<u8> {
        if let Some(slot) = self.registers.iter().position(|&r| r == register) {
            return Some(mirror_slot(slot));
        }
        if self.registers.len() + self.literals.len() >= MIRROR_SLOTS {
            return None;
        }
        self.registers.push(register);
        Some(mirror_slot(self.registers.len() - 1))
    }

    /// A register operand being read: guarded unless written earlier.
    fn read(&mut self, register: u16) -> Option<u8> {
        let slot = self.register(register)?;
        if self.written & (1 << slot) == 0 {
            self.guards |= 1 << slot;
        }
        Some(slot)
    }

    fn write(&mut self, register: u16) -> Option<u8> {
        let slot = self.register(register)?;
        self.written |= 1 << slot;
        Some(slot)
    }

    /// Literal slots are numbered after the register slots once the layout
    /// is complete; until then they carry a provisional index.
    fn literal(&mut self, bytecode: &Bytecode, index: u16) -> Option<u8> {
        if let Some(slot) = self.literal_ids.iter().position(|&id| id == index) {
            return Some(LITERAL_TAG | mirror_slot(slot));
        }
        if self.registers.len() + self.literals.len() >= MIRROR_SLOTS {
            return None;
        }
        let value = *bytecode.number_literals.get(usize::from(index))?;
        let slot = mirror_slot(self.literals.len());
        self.literals.push((slot, value));
        self.literal_ids.push(index);
        Some(LITERAL_TAG | slot)
    }
}

/// Marks a provisional literal operand while the layout is being built.
const LITERAL_TAG: u8 = 0x80;

/// A mirror slot index. The layout never grows past `MIRROR_SLOTS`, so it
/// fits a `u8` below `LITERAL_TAG`.
fn mirror_slot(index: usize) -> u8 {
    debug_assert!(index < MIRROR_SLOTS);
    (index % MIRROR_SLOTS) as u8
}

impl NumericLoop {
    fn recognize(bytecode: &Bytecode, prep: usize, cost: &CostState) -> Option<Self> {
        let code = &bytecode.code;
        let prep_inst = code[prep];
        // Both edges must close this loop: FOR_PREP skips exactly past
        // FOR_LOOP, and FOR_LOOP jumps back to the instruction after
        // FOR_PREP with the same controls. The verifier checks each
        // instruction on its own, so other shapes are admitted; they run
        // canonically.
        // Admission keeps every jump target inside the code and FOR_PREP's
        // offset forward, so these are the targets dispatch would take.
        let after = prep + 1;
        let exit = Frame::jumped(after, prep_inst.sj());
        if exit <= after {
            return None;
        }
        let step = exit - 1;
        let step_inst = *code.get(step)?;
        if step_inst.opcode() != Instr::OP_FOR_LOOP
            || step_inst.a() != prep_inst.a()
            || Frame::jumped(exit, step_inst.sj()) != after
        {
            return None;
        }

        let base = prep_inst.a();
        let mut layout = Layout {
            registers: Vec::new(),
            literals: Vec::new(),
            literal_ids: Vec::new(),
            written: 0,
            guards: 0,
        };
        for offset in 0..4 {
            layout.register(base.checked_add(offset)?)?;
        }
        let mut ops = Vec::with_capacity(step - after);
        let mut weights = Vec::with_capacity(step - after);
        for inst in &code[after..step] {
            let opcode = inst.opcode();
            let op = if let Some((op, operands)) = ArithOp::decode(opcode) {
                let (b, c) = match operands {
                    ArithOperands::RegReg => (layout.read(inst.b())?, layout.read(inst.c())?),
                    ArithOperands::RegConst => {
                        (layout.read(inst.b())?, layout.literal(bytecode, inst.c())?)
                    }
                    ArithOperands::ConstReg => {
                        (layout.literal(bytecode, inst.b())?, layout.read(inst.c())?)
                    }
                };
                LoopOp {
                    kind: OpKind::Arith(op),
                    a: layout.write(inst.a())?,
                    b,
                    c,
                }
            } else {
                match opcode {
                    Instr::OP_UNM => {
                        let b = layout.read(inst.b())?;
                        LoopOp {
                            kind: OpKind::Unm,
                            a: layout.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    Instr::OP_MOVE => {
                        let b = layout.read(inst.b())?;
                        LoopOp {
                            kind: OpKind::Copy,
                            a: layout.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    Instr::OP_LOAD_NUM => {
                        let b = layout.literal(bytecode, inst.b())?;
                        LoopOp {
                            kind: OpKind::Copy,
                            a: layout.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    _ => return None,
                }
            };
            ops.push(op);
            weights.push(Instr::fixed_charge(opcode).map_or(0, |charge| cost.weight(charge)));
        }
        // The loop step reads the three hidden controls after the body.
        for slot in 0..3u8 {
            if layout.written & (1 << slot) == 0 {
                layout.guards |= 1 << slot;
            }
        }

        // Number the literal slots after the registers.
        let first_literal = mirror_slot(layout.registers.len());
        let fix = |slot: u8| {
            if slot & LITERAL_TAG != 0 {
                first_literal + (slot & !LITERAL_TAG)
            } else {
                slot
            }
        };
        for op in &mut ops {
            op.b = fix(op.b);
            op.c = fix(op.c);
        }
        let literals = layout
            .literals
            .iter()
            .map(|&(slot, value)| (first_literal + slot, value))
            .collect::<Vec<_>>();

        let step_weight =
            Instr::fixed_charge(Instr::OP_FOR_LOOP).map_or(0, |charge| cost.weight(charge));
        let iteration_weight = weights
            .iter()
            .fold(step_weight, |sum, &weight| sum.saturating_add(weight));
        Some(Self {
            prep,
            registers: layout.registers.into_boxed_slice(),
            guards: layout.guards,
            literals: literals.into_boxed_slice(),
            ops: ops.into_boxed_slice(),
            weights: weights.into_boxed_slice(),
            step_weight,
            iteration_weight,
        })
    }

    /// The pc of the loop's `FOR_LOOP`.
    fn step_pc(&self) -> usize {
        self.prep + 1 + self.ops.len()
    }
}

/// Mask an operand into the mirror; every operand is below `MIRROR_SLOTS`
/// by construction, so this only spares the bounds check.
#[inline(always)]
fn at(slot: u8) -> usize {
    usize::from(slot) % MIRROR_SLOTS
}

impl State {
    /// Run the numeric region `region` of the frame at `base` after its
    /// `FOR_PREP` entered the loop, with the cursor `ip` just past
    /// `FOR_PREP`. Returns with `ip` past `FOR_LOOP` once the loop ends, or
    /// unchanged when the entry guard refuses (canonical dispatch then runs
    /// the loop). On an error `ip` names the instruction that failed, as
    /// canonical dispatch would.
    #[inline(never)]
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn run_numeric_loop(
        &mut self,
        region: &NumericLoop,
        base: usize,
        ip: &mut usize,
        local_cost: &mut u64,
    ) -> Result<()> {
        let mut x = [0.0f64; MIRROR_SLOTS];
        for (slot, &register) in region.registers.iter().enumerate() {
            if region.guards & (1 << slot) != 0 {
                match self.stack[base + usize::from(register)] {
                    Val::Num(n) => x[slot] = n,
                    _ => return Ok(()),
                }
            }
        }
        for &(slot, value) in &region.literals {
            x[at(slot)] = value;
        }

        let step_pc = region.step_pc();
        let mut written: u32 = 0;
        let mut published = false;
        let result = loop {
            if region.iteration_weight == 0
                || region.iteration_weight < self.cost_flush_at().saturating_sub(*local_cost)
            {
                if region.iteration_weight != 0 {
                    *local_cost += region.iteration_weight;
                    #[cfg(feature = "cost-check-interval")]
                    {
                        for (index, &weight) in region.weights.iter().enumerate() {
                            if weight != 0 {
                                crate::cost_meter::interval_test::record(
                                    region.prep + 2 + index,
                                    weight,
                                );
                            }
                        }
                        if region.step_weight != 0 {
                            crate::cost_meter::interval_test::record(
                                step_pc + 1,
                                region.step_weight,
                            );
                        }
                    }
                }
                for op in &region.ops {
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
            } else {
                let mut refused = None;
                for (index, op) in region.ops.iter().enumerate() {
                    let weight = region.weights[index];
                    if weight != 0 {
                        *ip = region.prep + 2 + index;
                        if let Err(error) = self.charge_dispatch(local_cost, *ip, weight) {
                            refused = Some(error);
                            break;
                        }
                    }
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
                if let Some(error) = refused {
                    break Err(error);
                }
                if region.step_weight != 0 {
                    *ip = step_pc + 1;
                    if let Err(error) = self.charge_dispatch(local_cost, *ip, region.step_weight) {
                        break Err(error);
                    }
                }
            }

            // FOR_LOOP, from the mirror's controls.
            *ip = step_pc + 1;
            let step = x[2];
            let next = x[0] + step;
            if !super::eval_control::check_numeric_for_condition(next, x[1], step) {
                break Ok(());
            }
            if !published {
                // Replace every value the first iteration overwrote before
                // the checkpoint can collect: from here on the written slots
                // hold numbers on the stack too.
                self.publish_mirror(region, base, &x, written);
                published = true;
            }
            if let Err(error) = self.poll_gc_checkpoint() {
                break Err(error);
            }
            x[0] = next;
            x[3] = next;
            written |= 0b1001;
            *ip = region.prep + 1;
        };
        self.publish_mirror(region, base, &x, written);
        result
    }

    fn publish_mirror(&mut self, region: &NumericLoop, base: usize, x: &[f64], written: u32) {
        for (slot, &register) in region.registers.iter().enumerate() {
            if written & (1 << slot) != 0 {
                self.stack[base + usize::from(register)] = Val::Num(x[slot]);
            }
        }
    }

    /// One fixed charge of `weight` (nonzero) at cursor `ip`, exactly as the
    /// dispatch loop's `add_cost!` makes it.
    fn charge_dispatch(&mut self, local_cost: &mut u64, ip: usize, weight: u64) -> Result<()> {
        #[cfg(not(feature = "cost-check-interval"))]
        let next = *local_cost + weight;
        #[cfg(feature = "cost-check-interval")]
        let next = (*local_cost).saturating_add(weight);
        if next >= self.cost_flush_at() {
            self.charge_cost(next)?;
            *local_cost = 0;
        } else {
            *local_cost = next;
        }
        #[cfg(feature = "cost-check-interval")]
        crate::cost_meter::interval_test::record(ip, weight);
        #[cfg(not(feature = "cost-check-interval"))]
        let _ = ip;
        Ok(())
    }
}

#[inline(always)]
fn eval(op: &LoopOp, x: &[f64; MIRROR_SLOTS]) -> f64 {
    let lhs = x[at(op.b)];
    let rhs = x[at(op.c)];
    match op.kind {
        OpKind::Arith(ArithOp::Add) => lua_add(lhs, rhs),
        OpKind::Arith(ArithOp::Sub) => lua_sub(lhs, rhs),
        OpKind::Arith(ArithOp::Mul) => lua_mul(lhs, rhs),
        OpKind::Arith(ArithOp::Div) => lua_div(lhs, rhs),
        OpKind::Arith(ArithOp::Mod) => lua_modulo(lhs, rhs),
        OpKind::Arith(ArithOp::Pow) => crate::crmath::pow(lhs, rhs),
        OpKind::Unm => -lhs,
        OpKind::Copy => lhs,
    }
}
