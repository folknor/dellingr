//! Straight-line number arithmetic runs in `f64` locals in two shapes:
//!
//! - **Typed loop regions.** A numeric `for` loop whose body is such
//!   arithmetic runs as one region: the registers the loop touches live in an
//!   `f64` mirror, and each iteration runs its body operations and the loop
//!   step without going back through the dispatch loop.
//! - **Numeric leaves.** A Lua function whose code up to its first `RETURN`
//!   is such arithmetic, returning one value, runs inside call dispatch
//!   without an activation when its arguments are numbers.
//!
//! Both are derived metadata built per State from admitted bytecode (fresh
//! compiles and snapshot loads alike), so nothing here is persisted or
//! admitted by the verifier. The canonical instructions stay in place and
//! remain the fallback: a loop or call the entry checks refuse, and any jump
//! into a body from elsewhere, runs them.
//!
//! Why a loop region is exact, observable boundary by observable boundary:
//!
//! - Types. A body holds only arithmetic, `UNM`, `MOVE`, `LOAD_NUM` and
//!   loop-invariant field reads, so numbers in give numbers out, and nothing
//!   else runs while the region does: no call, store, allocation, upvalue
//!   write or handler. The only foreign code is the back-edge checkpoint
//!   (`State::poll_gc_checkpoint`), which polls the interrupt and may
//!   collect, and runs no Lua or host code. A guard at entry on every
//!   register read before it is written in one pass of the body and the loop
//!   step therefore proves every read of the whole loop is a number. The
//!   guard reads operands before the destination is marked written, so
//!   `R0 = R0 + k` guards `R0`.
//! - Field reads. `GET_UPVAL r u; GET_FIELD r r k` (the scratch register
//!   reused as the destination) and `GET_FIELD d s k` with `s` a register
//!   the body never writes read a receiver that cannot change while the
//!   region runs: nothing in it writes an upvalue, a register outside the
//!   mirror or a table, a running closure's upvalues never alias its own
//!   frame's registers, and a collection frees dead objects without
//!   changing a live table's contents (the VM has no weak tables). So the
//!   field is read once at entry, through the site's own field cache as
//!   `GET_FIELD` reads it, and must be a raw present number; anything else
//!   (a miss that would consult a metatable, a non-table receiver, a
//!   non-number) declines the region. The pair's scratch register holds the
//!   table only between its two instructions, and every checkpoint and exit
//!   is at an instruction boundary after it, where it holds the number.
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
//!   completed prefix. Zero weights are skipped as dispatch skips them. A
//!   fused field read's two instructions are both free, so no refusal falls
//!   between them.
//! - The loop step reads the mirror's controls, which a body may have
//!   written, computes the next value into a temporary, polls on the taken
//!   back edge with the cursor past `FOR_LOOP`, and only then writes the
//!   hidden and visible counters, so an interrupted iteration writes no
//!   counter.
//!
//! Why a numeric leaf is exact: see `State::try_numeric_leaf`.

use super::Result;
use super::State;
use super::Val;
use super::eval::FRAME_HEADROOM;
use super::frame::Frame;
use crate::compiler::Bytecode;
use crate::cost_meter::CostState;
use crate::instr::{ArithOp, ArithOperands, Instr, RetCount};
use crate::limits::{MAX_CALL_DEPTH, MAX_STACK_SIZE};
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

/// Mirror slots per region or leaf: registers first, then constants. Code
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
    /// The `FOR_LOOP` pc. Fused field reads make the body shorter than its
    /// canonical instructions, so this is not `prep + 1 + ops.len()`.
    step_pc: usize,
    body: Body,
}

/// A straight-line body over an `f64` mirror.
#[derive(Debug)]
struct Body {
    /// Mirror slot -> frame register, for the register slots. In a loop,
    /// slots 0 to 3 are the controls (start/counter, limit, step, visible
    /// variable).
    registers: Box<[u16]>,
    /// Register slots loaded and checked at entry: read before written.
    guards: u32,
    /// Constant mirror slots and where their values come from, loaded at
    /// entry.
    constants: Box<[(u8, Constant)]>,
    ops: Box<[LoopOp]>,
    /// Each operation's fixed weight at this State's model, with the cursor
    /// (past its canonical instruction) dispatch charges it at.
    charges: Box<[(usize, u64)]>,
    /// The closing instruction's charge: `FOR_LOOP` for a loop, `RETURN`
    /// for a leaf.
    tail: (usize, u64),
    /// Sum of every charge of one pass, saturating.
    weight: u64,
}

#[derive(Clone, Copy, Debug)]
enum Constant {
    Number(f64),
    /// A loop-invariant own field, read at entry.
    Field {
        receiver: Receiver,
        /// The key's literal index.
        key: u16,
        /// The `GET_FIELD` site's cache slot.
        cache: Option<u8>,
    },
}

#[derive(Clone, Copy, Debug)]
enum Receiver {
    Upvalue(u16),
    Register(u16),
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
    /// `MOVE`, `LOAD_NUM` and invariant field reads (from a constant slot).
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

/// Builds a body's mirror layout.
#[derive(Default)]
struct Layout {
    registers: Vec<u16>,
    constants: Vec<Constant>,
    /// The literal index of each `Number` constant, for sharing.
    number_ids: Vec<Option<u16>>,
    written: u32,
    guards: u32,
    /// Registers read as field receivers; the body must never write them.
    receivers: Vec<u16>,
    ops: Vec<LoopOp>,
    charges: Vec<(usize, u64)>,
}

impl Layout {
    fn full(&self) -> bool {
        self.registers.len() + self.constants.len() >= MIRROR_SLOTS
    }

    fn register(&mut self, register: u16) -> Option<u8> {
        if let Some(slot) = self.registers.iter().position(|&r| r == register) {
            return Some(mirror_slot(slot));
        }
        if self.full() {
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

    /// Constant slots are numbered after the register slots once the layout
    /// is complete; until then they carry a provisional index.
    fn constant(&mut self, constant: Constant, number_id: Option<u16>) -> Option<u8> {
        if number_id.is_some()
            && let Some(slot) = self.number_ids.iter().position(|&id| id == number_id)
        {
            return Some(CONSTANT_TAG | mirror_slot(slot));
        }
        if self.full() {
            return None;
        }
        self.constants.push(constant);
        self.number_ids.push(number_id);
        Some(CONSTANT_TAG | mirror_slot(self.constants.len() - 1))
    }

    fn number(&mut self, bytecode: &Bytecode, index: u16) -> Option<u8> {
        let value = *bytecode.number_literals.get(usize::from(index))?;
        self.constant(Constant::Number(value), Some(index))
    }

    /// `opcode`'s fixed weight at the cursor past `pc`.
    fn charge(cost: &CostState, opcode: u8, pc: usize) -> (usize, u64) {
        let weight = Instr::fixed_charge(opcode).map_or(0, |charge| cost.weight(charge));
        (pc + 1, weight)
    }

    /// Scan the straight-line instructions of `code` from `pc` up to `end`
    /// (exclusive) or, when `end` is `None`, up to the first instruction
    /// that is not a body operation. Returns the pc where the scan stopped,
    /// or `None` on an instruction the body cannot hold when `end` is set.
    /// `fields` admits the invariant field reads (loop regions only).
    fn scan(
        &mut self,
        bytecode: &Bytecode,
        cost: &CostState,
        mut pc: usize,
        end: Option<usize>,
        fields: bool,
    ) -> Option<usize> {
        let code = &bytecode.code;
        while end.is_none_or(|end| pc < end) {
            let inst = *code.get(pc)?;
            let opcode = inst.opcode();
            let op = if let Some((op, operands)) = ArithOp::decode(opcode) {
                let (b, c) = match operands {
                    ArithOperands::RegReg => (self.read(inst.b())?, self.read(inst.c())?),
                    ArithOperands::RegConst => {
                        (self.read(inst.b())?, self.number(bytecode, inst.c())?)
                    }
                    ArithOperands::ConstReg => {
                        (self.number(bytecode, inst.b())?, self.read(inst.c())?)
                    }
                };
                LoopOp {
                    kind: OpKind::Arith(op),
                    a: self.write(inst.a())?,
                    b,
                    c,
                }
            } else {
                match opcode {
                    Instr::OP_UNM => {
                        let b = self.read(inst.b())?;
                        LoopOp {
                            kind: OpKind::Unm,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    Instr::OP_MOVE => {
                        let b = self.read(inst.b())?;
                        LoopOp {
                            kind: OpKind::Copy,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    Instr::OP_LOAD_NUM => {
                        let b = self.number(bytecode, inst.b())?;
                        LoopOp {
                            kind: OpKind::Copy,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    // `GET_UPVAL r u; GET_FIELD r r k`, both inside the body.
                    Instr::OP_GET_UPVAL
                        if fields
                            && end.is_some_and(|end| pc + 1 < end)
                            && code.get(pc + 1).is_some_and(|field| {
                                field.opcode() == Instr::OP_GET_FIELD
                                    && field.a() == inst.a()
                                    && field.b() == inst.a()
                            }) =>
                    {
                        let field = code[pc + 1];
                        let b = self.constant(
                            Constant::Field {
                                receiver: Receiver::Upvalue(inst.b()),
                                key: field.c(),
                                cache: field.cache_slot(),
                            },
                            None,
                        )?;
                        // Both reads are free, so the pair has no charge
                        // and no refusal can fall between its halves, where
                        // the scratch register would hold the table.
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_GET_FIELD).is_none());
                        }
                        pc += 1;
                        self.charges.push((pc + 1, 0));
                        let a = self.write(field.a())?;
                        self.ops.push(LoopOp {
                            kind: OpKind::Copy,
                            a,
                            b,
                            c: 0,
                        });
                        pc += 1;
                        continue;
                    }
                    Instr::OP_GET_FIELD if fields => {
                        self.receivers.push(inst.b());
                        let b = self.constant(
                            Constant::Field {
                                receiver: Receiver::Register(inst.b()),
                                key: inst.c(),
                                cache: inst.cache_slot(),
                            },
                            None,
                        )?;
                        LoopOp {
                            kind: OpKind::Copy,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    _ if end.is_none() => return Some(pc),
                    _ => return None,
                }
            };
            self.ops.push(op);
            self.charges.push(Self::charge(cost, opcode, pc));
            pc += 1;
        }
        Some(pc)
    }

    /// The finished body. `None` if a field receiver register is written by
    /// the body or is one of the first `reserved` registers (loop controls).
    fn finish(mut self, reserved: usize, tail: (usize, u64)) -> Option<(Body, Vec<u16>)> {
        for &receiver in &self.receivers {
            if let Some(slot) = self.registers.iter().position(|&r| r == receiver)
                && (slot < reserved || self.written & (1 << slot) != 0)
            {
                return None;
            }
        }
        // Number the constant slots after the registers. The layout never
        // holds more than `MIRROR_SLOTS` slots in all, so every final slot is
        // below it, while `first` alone may equal it when there are none.
        let first = self.registers.len() as u8;
        let fix = |slot: u8| {
            if slot & CONSTANT_TAG != 0 {
                first + (slot & !CONSTANT_TAG)
            } else {
                slot
            }
        };
        for op in &mut self.ops {
            op.b = fix(op.b);
            op.c = fix(op.c);
        }
        let constants = self
            .constants
            .iter()
            .enumerate()
            .map(|(index, &constant)| (first + mirror_slot(index), constant))
            .collect::<Vec<_>>();
        let weight = self
            .charges
            .iter()
            .fold(tail.1, |sum, &(_, weight)| sum.saturating_add(weight));
        let registers = self.registers.clone();
        Some((
            Body {
                registers: self.registers.into_boxed_slice(),
                guards: self.guards,
                constants: constants.into_boxed_slice(),
                ops: self.ops.into_boxed_slice(),
                charges: self.charges.into_boxed_slice(),
                tail,
                weight,
            },
            registers,
        ))
    }
}

impl Body {
    /// Record every positive charge of one pass in the charge-site trace,
    /// as per-instruction dispatch would.
    #[cfg(feature = "cost-check-interval")]
    fn record_charges(&self) {
        for &(cursor, weight) in self.charges.iter().chain(std::iter::once(&self.tail)) {
            if weight != 0 {
                crate::cost_meter::interval_test::record(cursor, weight);
            }
        }
    }
}

/// Marks a provisional constant operand while the layout is being built.
const CONSTANT_TAG: u8 = 0x80;

/// A mirror slot index. The layout never grows past `MIRROR_SLOTS`, so it
/// fits a `u8` below `CONSTANT_TAG`.
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
        let step_pc = exit - 1;
        let step_inst = *code.get(step_pc)?;
        if step_inst.opcode() != Instr::OP_FOR_LOOP
            || step_inst.a() != prep_inst.a()
            || Frame::jumped(exit, step_inst.sj()) != after
        {
            return None;
        }

        let base = prep_inst.a();
        let mut layout = Layout::default();
        for offset in 0..4 {
            layout.register(base.checked_add(offset)?)?;
        }
        layout.scan(bytecode, cost, after, Some(step_pc), true)?;
        let tail = Layout::charge(cost, Instr::OP_FOR_LOOP, step_pc);
        // The loop step reads the three hidden controls after the body.
        for slot in 0..3u8 {
            if layout.written & (1 << slot) == 0 {
                layout.guards |= 1 << slot;
            }
        }
        let (body, _) = layout.finish(4, tail)?;
        Some(Self {
            prep,
            step_pc,
            body,
        })
    }
}

/// A function whose code from pc 0 to its first `RETURN` is a numeric body
/// returning one register, run by `State::try_numeric_leaf`.
#[derive(Debug)]
pub(super) struct NumericLeaf {
    body: Body,
    /// The returned register's mirror slot.
    result: u8,
    /// The callee's frame size, which admission would reserve.
    max_stack: usize,
}

impl NumericLeaf {
    /// The leaf plan of `bytecode`, with fixed weights from `cost`. Every
    /// register the body reads before writing must be a parameter: any other
    /// is nil at admission, so canonical execution would fail or return nil,
    /// which the plan does not model.
    pub(super) fn recognize(bytecode: &Bytecode, cost: &CostState) -> Option<Self> {
        if bytecode.is_vararg {
            return None;
        }
        let mut layout = Layout::default();
        let ret_pc = layout.scan(bytecode, cost, 0, None, false)?;
        let ret = *bytecode.code.get(ret_pc)?;
        if ret.opcode() != Instr::OP_RETURN || ret.return_count() != RetCount::Fixed(1) {
            return None;
        }
        // The returned register is a read: `return x` must see a number.
        let result = layout.read(ret.a())?;
        let tail = Layout::charge(cost, Instr::OP_RETURN, ret_pc);
        let (body, registers) = layout.finish(0, tail)?;
        for (slot, &register) in registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 && register >= u16::from(bytecode.num_params) {
                return None;
            }
        }
        Some(Self {
            body,
            result,
            max_stack: usize::from(bytecode.max_stack),
        })
    }
}

/// Mask an operand into the mirror; every operand is below `MIRROR_SLOTS`
/// by construction, so this only spares the bounds check.
#[inline(always)]
fn at(slot: u8) -> usize {
    usize::from(slot) % MIRROR_SLOTS
}

impl State {
    /// Run the numeric region `region` of `frame` after its `FOR_PREP`
    /// entered the loop, with the cursor `ip` just past `FOR_PREP`. Returns
    /// with `ip` past `FOR_LOOP` once the loop ends, or unchanged when the
    /// entry checks refuse (canonical dispatch then runs the loop).
    /// On an error `ip` names the instruction that failed, as canonical
    /// dispatch would.
    #[inline(never)]
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn run_numeric_loop(
        &mut self,
        region: &NumericLoop,
        frame: &Frame,
        ip: &mut usize,
        local_cost: &mut u64,
    ) -> Result<()> {
        let base = frame.stack_bottom;
        let body = &region.body;
        let mut x = [0.0f64; MIRROR_SLOTS];
        if !self.load_mirror(body, frame, base, &mut x) {
            return Ok(());
        }

        let step_pc = region.step_pc;
        let mut written: u32 = 0;
        let mut published = false;
        let result = loop {
            if body.weight == 0 || body.weight < self.cost_flush_at().saturating_sub(*local_cost) {
                if body.weight != 0 {
                    *local_cost += body.weight;
                    #[cfg(feature = "cost-check-interval")]
                    body.record_charges();
                }
                for op in &body.ops {
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
            } else {
                let mut refused = None;
                for (op, &(cursor, weight)) in body.ops.iter().zip(&body.charges) {
                    if weight != 0 {
                        *ip = cursor;
                        if let Err(error) = self.charge_dispatch(local_cost, cursor, weight) {
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
                let (cursor, weight) = body.tail;
                if weight != 0 {
                    *ip = cursor;
                    if let Err(error) = self.charge_dispatch(local_cost, cursor, weight) {
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
                self.publish_mirror(body, base, &x, written);
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
        self.publish_mirror(body, base, &x, written);
        result
    }

    /// Load `body`'s guarded registers of the frame at `base` and its
    /// constants into `x`. `false` if a guard or a field read refuses.
    fn load_mirror(
        &self,
        body: &Body,
        frame: &Frame,
        base: usize,
        x: &mut [f64; MIRROR_SLOTS],
    ) -> bool {
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                match self.stack[base + usize::from(register)] {
                    Val::Num(n) => x[slot] = n,
                    _ => return false,
                }
            }
        }
        for &(slot, constant) in &body.constants {
            x[at(slot)] = match constant {
                Constant::Number(value) => value,
                Constant::Field {
                    receiver,
                    key,
                    cache,
                } => {
                    let receiver = match receiver {
                        Receiver::Upvalue(index) => self.upvalue_value(frame, index),
                        Receiver::Register(register) => self.stack[base + usize::from(register)],
                    };
                    let Val::Obj(ptr) = receiver else {
                        return false;
                    };
                    let cache = frame.runtime.caches.field(cache);
                    match self.get_table_field_direct(ptr, frame.literal(key), cache) {
                        Some((Some(Val::Num(n)), _)) => n,
                        _ => return false,
                    }
                }
            };
        }
        true
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

    fn publish_mirror(&mut self, body: &Body, base: usize, x: &[f64], written: u32) {
        for (slot, &register) in body.registers.iter().enumerate() {
            if written & (1 << slot) != 0 {
                self.stack[base + usize::from(register)] = Val::Num(x[slot]);
            }
        }
    }

    /// Run the call `OP_CALL` is making at function slot `idx` with
    /// `num_args` arguments as a numeric leaf, without admitting an
    /// activation, if its callee has a leaf plan and the call would succeed
    /// canonically with nothing observable in between. `false`, with nothing
    /// changed, otherwise; canonical admission then runs the call.
    ///
    /// Exact because every check canonical admission and the callee would
    /// make is made first, and declines rather than fails:
    /// - the call depth is below `MAX_CALL_DEPTH`;
    /// - the poll flag is clear, so admission's checkpoint would neither
    ///   interrupt nor collect (this read is the leaf's admission safepoint);
    /// - the frame reservation, the callee's registers plus
    ///   `FRAME_HEADROOM`, fits under the stack cap;
    /// - every register the body reads before writing is an argument holding
    ///   a number, so no instruction can fail;
    /// - the callee's charges, body then `RETURN`, sum strictly below the
    ///   flush threshold minus the pending batch, so adding them at once
    ///   leaves the batch where per-instruction charging would.
    ///
    /// The body runs no call, allocation, upvalue access or capture, so
    /// nothing can observe the missing activation: no traceback, call-site
    /// record, collection or handler runs while it would exist. The result
    /// lands in the function slot and the stack is cut above it, as the
    /// return slides it; the `OP_CALL` arm's resize then supplies a fixed
    /// count's missing results, as after a return.
    ///
    /// Out of line: `call_from_lua`, its caller, stays live across a Rust
    /// callee's re-entry, so its frame must not hold this one's mirror.
    #[inline(never)]
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn try_numeric_leaf(
        &mut self,
        idx: usize,
        num_args: u8,
        expected: RetCount,
        local_cost: &mut u64,
    ) -> bool {
        // Off under fault injection, whose call-depth and stack-space sites
        // count every admission.
        if cfg!(feature = "fault-injection") {
            return false;
        }
        let Val::Obj(ptr) = self.stack[idx] else {
            return false;
        };
        let Some(closure) = self.heap.as_lua_function_ref(ptr) else {
            return false;
        };
        let Some(leaf) = closure.runtime.numeric_leaf.as_ref() else {
            return false;
        };
        let body = &leaf.body;
        if self.in_flight.call_depth >= MAX_CALL_DEPTH
            || self.heap.gc_poll_requested()
            || idx + 1 + leaf.max_stack + FRAME_HEADROOM > MAX_STACK_SIZE
            || (body.weight != 0 && body.weight >= self.cost_flush_at().saturating_sub(*local_cost))
        {
            return false;
        }
        let mut x = [0.0f64; MIRROR_SLOTS];
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                if register >= u16::from(num_args) {
                    return false;
                }
                match self.stack[idx + 1 + usize::from(register)] {
                    Val::Num(n) => x[slot] = n,
                    _ => return false,
                }
            }
        }
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                x[at(slot)] = value;
            }
        }
        for op in &body.ops {
            x[at(op.a)] = eval(op, &x);
        }
        let result = x[at(leaf.result)];
        let weight = body.weight;
        #[cfg(feature = "cost-check-interval")]
        if weight != 0 {
            body.record_charges();
        }
        *local_cost += weight;
        if expected == RetCount::Fixed(0) {
            self.stack.truncate(idx);
        } else {
            self.stack[idx] = Val::Num(result);
            self.stack.truncate(idx + 1);
        }
        true
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

#[cfg(test)]
mod tests {
    use super::{NumericLeaf, NumericLoops};
    use crate::cost_meter::CostState;
    use crate::{CostModel, Costing};

    fn cost() -> CostState {
        CostState::new(Costing::Metered(CostModel::default()))
    }

    /// The loops of the first nested function of `source` that run as
    /// regions, by `FOR_PREP` pc.
    fn regions(source: &str) -> usize {
        let chunk = crate::compiler::compile(source, None).expect("compiles");
        NumericLoops::new(&chunk.nested[0], &cost()).loops.len()
    }

    fn leaf(source: &str) -> bool {
        let chunk = crate::compiler::compile(source, None).expect("compiles");
        NumericLeaf::recognize(&chunk.nested[0], &cost()).is_some()
    }

    #[test]
    fn regions_take_arithmetic_and_invariant_field_reads() {
        assert_eq!(
            regions(
                "local function f() local s = 0 for i = 1, 9 do s = s + i * 2.5 - i / 3 end end"
            ),
            1
        );
        let fields = "local e = {x = 1}
            local function f(o)
                local s = 0
                for i = 1, 9 do s = s + e.x + o.y end
                return s
            end";
        assert_eq!(regions(fields), 1);
        // A receiver the body writes is not invariant.
        assert_eq!(
            regions("local function f(o) local s = 0 for i = 1, 9 do s = s + o.y; o = i end end"),
            0
        );
    }

    #[test]
    fn leaves_are_numeric_bodies_over_parameters() {
        assert!(leaf("local function f(x) return x + 1 end"));
        assert!(leaf(
            "local function f(x, y) local t = x * 2 - y return -t end"
        ));
        assert!(leaf("local function f(x) return x end"));
        assert!(!leaf("local function f(...) return 1 end"));
        assert!(!leaf(
            "local function f(x) if x then return 1 end return 2 end"
        ));
        assert!(!leaf("local function f(x) return x, x end"));
        // 31 parameters and one temporary fill the mirror with no constants.
        let params = (0..31).map(|i| format!("p{i}")).collect::<Vec<_>>();
        let sum = params.join(" + ");
        assert!(leaf(&format!(
            "local function f({}) return {sum} end",
            params.join(", ")
        )));
    }
}
