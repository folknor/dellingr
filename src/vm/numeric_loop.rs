//! Straight-line number arithmetic runs in `f64` locals in two shapes:
//!
//! - **Typed loop regions.** A numeric `for` loop whose body is such
//!   arithmetic runs as one region: the registers the loop touches live in an
//!   `f64` mirror, and each iteration runs its body operations and the loop
//!   step without going back through the dispatch loop. A body without
//!   calls may also branch forward on number comparisons and read
//!   elements of loop-invariant tables (`State::run_control_region`).
//! - **Numeric leaves.** A Lua function whose code up to its first `RETURN`
//!   is such arithmetic, returning one value or none, runs inside call
//!   dispatch without an activation when its arguments are numbers. Its
//!   body may also read and store string-keyed fields of receiver tables (a
//!   parameter used only as a receiver, or an upvalue), the method shape
//!   `self.count = self.count + n`; such a field leaf runs when every field
//!   is raw present in its table. A loop region may call either kind,
//!   through a global, an upvalue or a method of a loop-invariant receiver.
//!
//! Both are derived metadata built per State from admitted bytecode (fresh
//! compiles and snapshot loads alike), so nothing here is persisted or
//! admitted by the verifier. The canonical instructions stay in place and
//! remain the fallback: a loop or call the entry checks refuse, and any jump
//! into a body from elsewhere, runs them.
//!
//! Why a loop region is exact, observable boundary by observable boundary:
//!
//! - Types. A body holds only arithmetic, `UNM`, `MOVE`, `LOAD_NUM`,
//!   loop-invariant field reads and calls to numeric leaves, so numbers in
//!   give numbers out, and nothing else runs while the region does: no
//!   other call, allocation, upvalue write or handler, and no store but a
//!   field leaf's number into a slot already live. The only foreign code is the back-edge checkpoint
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
//!   mirror or a table field under the read's key (a called field leaf's
//!   store keys are checked against it at entry), a running closure's
//!   upvalues never alias its own
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
//! - Calls. A body may call numeric leaves: `GET_GLOBAL r k`, `GET_UPVAL
//!   r u`, or a method lookup `SELF r s k` on a receiver the body never
//!   writes (an upvalue read by the `GET_UPVAL s u` before it, with `s` one
//!   of `SELF`'s destinations, or a register), then `CALL r` with fixed
//!   arguments and zero or one result, nothing else touching `r` (or a
//!   method's receiver copy in `r + 1`) in between. The callee, a method's
//!   receiver and route, and a field leaf's slots are resolved once at
//!   entry (`State::resolve_callees`, which says why each stays valid);
//!   the callee must be a Lua closure with a leaf plan, and the binding
//!   keeps it reachable. Each call runs the leaf on the mirror, a field
//!   leaf reloading and overwriting its slots, writes nil into every
//!   register the call clears up to the frame's top (above `r`, and `r`
//!   too for no result) as a fixed-result call leaves them, and records
//!   its call site. A register read before written must lie below every
//!   cleared one, or the second pass would read that nil. Each call reads
//!   the poll flag first, as its admission would poll, and when the flag
//!   is set, or in a per-operation pass when the leaf's charges might be
//!   refused, the region exits at the `CALL` with the callee back in `r`
//!   and a method's receiver in `r + 1` (installed after the mirror is
//!   published) and canonical dispatch makes the call, so no leaf effect
//!   happens before every one of its charges is admitted. A fast pass adds
//!   the charges of the operations before the exit only.
//! - The loop step reads the mirror's controls, which a body may have
//!   written, computes the next value into a temporary, polls on the taken
//!   back edge with the cursor past `FOR_LOOP`, and only then writes the
//!   hidden and visible counters, so an interrupted iteration writes no
//!   counter.
//!
//! Why a numeric leaf is exact: see `State::try_numeric_leaf`.

use std::sync::Arc;

use super::BytecodeRuntime;
use super::MetaName;
use super::Result;
use super::State;
use super::Val;
use super::caches::{FieldLookupCacheEntry, FieldLookupCacheSlot, SetFieldLookupCacheSlot};
use super::eval::FRAME_HEADROOM;
use super::frame::Frame;
use super::object::{ObjectPtr, Upvalue};
use super::table::SlotRef;
use crate::compiler::Bytecode;
use crate::cost_meter::CostState;
use crate::instr::{ArgCount, ArithOp, ArithOperands, CompareOp, Instr, RetCount};
use crate::limits::{MAX_CALL_DEPTH, MAX_METAMETHOD_DEPTH, MAX_STACK_SIZE};
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
    /// Sum of every charge of one pass, saturating; a loop's leaf calls add
    /// their callees' weights at entry.
    weight: u64,
    /// A loop's calls to numeric leaves, in body order. Empty for a leaf.
    calls: Box<[CallSite]>,
    /// A loop's indexed-read receivers, with the pc an exit at each read
    /// resumes canonical dispatch from. Empty for a leaf.
    indexed: Box<[(Receiver, usize)]>,
    /// The body has forward branches or indexed reads, so it runs through
    /// `State::run_control_region` rather than as a straight line.
    control: bool,
}

/// Indexed reads one loop body may make.
const MAX_INDEXED: usize = 8;

/// Calls one loop body may make to numeric leaves.
const MAX_CALLS: usize = 4;

/// A loop body's `GET_GLOBAL r k`, `GET_UPVAL r u` or method lookup into
/// `r`, then its `CALL r` with fixed arguments and zero or one result, made
/// to a numeric leaf read once at entry.
#[derive(Debug)]
struct CallSite {
    callee: Callee,
    /// The function register `r`.
    register: u16,
    /// `r`'s mirror slot, which holds the call's result when it has one.
    slot: u8,
    /// The argument registers' mirror slots, upwards from `r + 1`. A method
    /// call's first, the receiver in `r + 1`, is a placeholder 0.
    args: Box<[u8]>,
    /// Results the call keeps: 0 or 1.
    results: u8,
    /// Mirror slots of the registers the call leaves nil: above `r`, and
    /// `r` itself when it keeps no result.
    above: u32,
    /// The `CALL` pc.
    pc: usize,
    /// The `Call` operation's index in the body.
    op: usize,
}

impl CallSite {
    /// The first register the call leaves nil.
    fn cleared(&self) -> u16 {
        self.register + u16::from(self.results)
    }
}

#[derive(Clone, Copy, Debug)]
enum Callee {
    Global {
        name: u16,
        cache: Option<u8>,
    },
    Upvalue(u16),
    /// `SELF r s k` (after `GET_UPVAL s u` when the receiver is an upvalue):
    /// method `k` of a loop-invariant receiver table, with the receiver in
    /// `r + 1` as the call's first argument.
    Method {
        receiver: Receiver,
        key: u16,
    },
}

impl Callee {
    fn method(self) -> bool {
        matches!(self, Callee::Method { .. })
    }
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

/// Where a field leaf's receiver table comes from: a parameter register
/// used only as a receiver, or one of the running closure's upvalues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableSource {
    Param(u16),
    Upvalue(u16),
}

/// Most distinct field sites one leaf may touch.
const MAX_FIELD_SITES: usize = 16;

/// A field leaf's sites resolved to live slots, in site order.
type FieldSlots = [Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES];

/// One field a leaf reads or stores: a receiver source and a key literal,
/// shared by every `GET_FIELD` and `SET_FIELD` of the body that names both.
#[derive(Clone, Copy, Debug)]
struct FieldSite {
    source: TableSource,
    /// The key's literal index.
    key: u16,
    /// A constituent `GET_FIELD`'s own-field cache slot, if any.
    read_cache: Option<u8>,
    /// A constituent `SET_FIELD`'s set cache slot, if any.
    set_cache: Option<u8>,
    /// Some constituent reads it, so it must hold a number at entry.
    number: bool,
    /// Some constituent stores to it, so its key must not be `__index`.
    store: bool,
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
    /// The callee load of call site `b`: writes no mirror slot.
    Load,
    /// Call site `b`, its result into slot `a`.
    Call,
    /// A field leaf's read of field site `b` into slot `a`.
    Read,
    /// A field leaf's store of slot `c` into field site `b`: writes no
    /// mirror slot.
    Store,
    /// A loop's forward compare-branch: when `b` compared with `c` is
    /// false, continue at operation index `a`. Writes no mirror slot.
    Branch(CompareOp),
    /// A loop's forward `JMP` to operation index `a`. Writes no mirror slot.
    Jump,
    /// A loop's raw read of indexed receiver `b` at the key in slot `c`
    /// into slot `a`, exiting to canonical dispatch unless it is a number.
    Index,
}

/// What a scan admits besides arithmetic.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A loop body: invariant field reads read at entry, and calls to
    /// numeric leaves.
    Region,
    /// A function body: field reads and stores on receiver tables, executed
    /// in place.
    Leaf,
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
    /// Slots some instruction of the body writes, on any path.
    written: u32,
    /// Slots written on every path from the body's head to the instruction
    /// being scanned: a read outside this set is guarded at entry.
    must: u32,
    /// The instruction being scanned is reached only by a jump: the one
    /// before it does not fall through.
    dead: bool,
    /// Forward branches not yet reached: target pc, `must` at the branch.
    pending: Vec<(usize, u32)>,
    /// The operation index each scanned instruction starts, by pc.
    op_at: Vec<(usize, usize)>,
    /// Branch and jump operations with their target pcs, resolved to
    /// operation indices when the scan ends.
    jumps: Vec<(usize, usize)>,
    /// Indexed-read receivers and their exit pcs.
    indexed: Vec<(Receiver, usize)>,
    guards: u32,
    /// Registers read as field receivers; the body must never write them.
    receivers: Vec<u16>,
    ops: Vec<LoopOp>,
    charges: Vec<(usize, u64)>,
    calls: Vec<CallSite>,
    /// A callee loaded into its register and not yet called: no other
    /// operation may touch that register in between.
    loading: Option<(u16, Callee)>,
    /// The lowest function register called so far: the loop controls must
    /// lie below it.
    call_floor: Option<u16>,
    /// The lowest register a call leaves nil: above its function register,
    /// and the function register itself when the call keeps no result. A
    /// register at or above it reads nil unless it is in `revived`.
    nil_from: Option<u16>,
    /// Registers at or above `nil_from` written since the call that cleared
    /// them.
    revived: Vec<u16>,
    /// Registers a method lookup writes (`SELF`'s function register and
    /// receiver copy): a field or indexed receiver must not be one.
    method_writes: Vec<u16>,
    /// A leaf's registers whose reaching definition is a table, with its
    /// source. A numeric read of one refuses the plan.
    tables: Vec<(u16, TableSource)>,
    /// A leaf's field sites.
    sites: Vec<FieldSite>,
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
        if self.untouchable(register)
            || self.tables.iter().any(|&(r, _)| r == register)
            || self
                .nil_from
                .is_some_and(|from| register >= from && !self.revived.contains(&register))
        {
            return None;
        }
        let slot = self.register(register)?;
        if self.must & (1 << slot) == 0 {
            self.guards |= 1 << slot;
        }
        Some(slot)
    }

    fn write(&mut self, register: u16) -> Option<u8> {
        if self.untouchable(register) {
            return None;
        }
        let slot = self.register(register)?;
        self.written |= 1 << slot;
        self.must |= 1 << slot;
        self.tables.retain(|&(r, _)| r != register);
        if self.nil_from.is_some_and(|from| register >= from) && !self.revived.contains(&register) {
            self.revived.push(register);
        }
        Some(slot)
    }

    /// The register of a callee loaded and not yet called, and a method
    /// call's receiver copy above it.
    fn untouchable(&self, register: u16) -> bool {
        self.loading.is_some_and(|(loaded, callee)| {
            loaded == register || (callee.method() && loaded.checked_add(1) == Some(register))
        })
    }

    /// The table source `register` holds where a leaf uses it as a field
    /// receiver: its reaching definition's, or, for a register the body has
    /// not yet read or written as a number, the parameter itself (checked to
    /// be a parameter when the plan is finished). A parameter so used is a
    /// table from then on, so a later numeric read of it refuses.
    fn receiver(&mut self, register: u16) -> Option<TableSource> {
        if let Some(&(_, source)) = self.tables.iter().find(|&&(r, _)| r == register) {
            return Some(source);
        }
        if self.registers.contains(&register) {
            return None;
        }
        let source = TableSource::Param(register);
        self.tables.push((register, source));
        Some(source)
    }

    /// The field site of `source` and `key`, shared with earlier
    /// constituents naming both.
    fn site(
        &mut self,
        source: TableSource,
        key: u16,
        cache: Option<u8>,
        store: bool,
    ) -> Option<u8> {
        let index = match self
            .sites
            .iter()
            .position(|site| site.source == source && site.key == key)
        {
            Some(index) => index,
            None => {
                if self.sites.len() >= MAX_FIELD_SITES {
                    return None;
                }
                self.sites.push(FieldSite {
                    source,
                    key,
                    read_cache: None,
                    set_cache: None,
                    number: false,
                    store: false,
                });
                self.sites.len() - 1
            }
        };
        let site = &mut self.sites[index];
        if store {
            site.store = true;
            site.set_cache = site.set_cache.or(cache);
        } else {
            site.number = true;
            site.read_cache = site.read_cache.or(cache);
        }
        Some(index as u8)
    }

    /// `GET_GLOBAL r k`, `GET_UPVAL r u` or a method lookup loading a
    /// callee into `r` (and a method's receiver into `r + 1`). Off under
    /// fault injection, whose call-depth and stack-space sites count every
    /// admission.
    fn load(&mut self, register: u16, callee: Callee) -> Option<LoopOp> {
        if cfg!(feature = "fault-injection")
            || self.loading.is_some()
            || self.calls.len() >= MAX_CALLS
            || self.untouchable(register)
        {
            return None;
        }
        if callee.method() {
            let copy = register.checked_add(1)?;
            self.method_writes.extend([register, copy]);
        }
        self.register(register)?;
        self.loading = Some((register, callee));
        Some(LoopOp {
            kind: OpKind::Load,
            a: 0,
            b: self.calls.len() as u8,
            c: 0,
        })
    }

    /// `CALL r` of the loaded callee, with fixed arguments and zero or one
    /// result.
    fn call_site(&mut self, inst: Instr, pc: usize) -> Option<LoopOp> {
        // An exit at the call leaves canonical dispatch to charge `CALL`
        // again, which only a free `CALL` makes exact.
        const {
            assert!(Instr::fixed_charge(Instr::OP_CALL).is_none());
        }
        let (register, callee) = self.loading?;
        let ArgCount::Fixed(count) = inst.call_args() else {
            return None;
        };
        let results = match inst.call_results() {
            RetCount::Fixed(0) => 0,
            RetCount::Fixed(1) => 1,
            _ => return None,
        };
        let method = callee.method();
        if inst.a() != register || inst.call_is_method() != method || (method && count == 0) {
            return None;
        }
        // A method call's first argument is the receiver copy, which is a
        // table, not a mirror slot: its entry is a placeholder no leaf reads
        // (`State::resolve_callees` refuses a numeric read of parameter 0).
        let mut args = Vec::with_capacity(usize::from(count));
        for offset in 1..=u16::from(count) {
            args.push(if method && offset == 1 {
                0
            } else {
                self.read(register.checked_add(offset)?)?
            });
        }
        self.loading = None;
        // Everything above `r`, and `r` too without a result, is nil after
        // the call.
        let cleared = register.checked_add(u16::from(results))?;
        self.call_floor = Some(
            self.call_floor
                .map_or(register, |floor| floor.min(register)),
        );
        self.nil_from = Some(self.nil_from.map_or(cleared, |from| from.min(cleared)));
        self.revived.retain(|&revived| revived < cleared);
        let a = if results == 1 {
            self.write(register)?
        } else {
            self.register(register)?
        };
        let index = self.calls.len() as u8;
        self.calls.push(CallSite {
            callee,
            register,
            slot: a,
            args: args.into_boxed_slice(),
            results,
            above: 0,
            pc,
            op: self.ops.len(),
        });
        Some(LoopOp {
            kind: OpKind::Call,
            a,
            b: index,
            c: 0,
        })
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

    /// Enter the instruction at `pc`: `must` becomes the intersection over
    /// every edge reaching it, the fall-through from the instruction before
    /// unless that one jumped away, and each forward branch targeting `pc`.
    /// `None` for code no edge reaches.
    fn join(&mut self, pc: usize) -> Option<()> {
        let mut reached = !self.dead;
        let mut must = if self.dead { u32::MAX } else { self.must };
        self.pending.retain(|&(target, at_branch)| {
            if target == pc {
                reached = true;
                must &= at_branch;
                false
            } else {
                true
            }
        });
        if !reached {
            return None;
        }
        self.must = must;
        self.dead = false;
        Some(())
    }

    /// A forward branch or jump at `pc` with offset `offset`, inside a body
    /// ending at `end`; its target joins `must` as it stands now.
    fn branch(&mut self, pc: usize, offset: i32, end: Option<usize>) -> Option<()> {
        let end = end?;
        let target = Frame::jumped(pc + 1, offset);
        if target <= pc || target > end {
            return None;
        }
        self.pending.push((target, self.must));
        self.jumps.push((self.ops.len(), target));
        Some(())
    }

    /// An indexed read of `receiver` by `GET_TABLE` instruction `inst`,
    /// whose exit resumes canonical dispatch at `exit`. The key is read
    /// before the destination is written, as dispatch reads it.
    fn index(&mut self, receiver: Receiver, exit: usize, inst: Instr) -> Option<LoopOp> {
        if self.indexed.len() >= MAX_INDEXED {
            return None;
        }
        let c = self.read(inst.c())?;
        let a = self.write(inst.a())?;
        self.indexed.push((receiver, exit));
        Some(LoopOp {
            kind: OpKind::Index,
            a,
            b: (self.indexed.len() - 1) as u8,
            c,
        })
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
    /// `mode` says which field and call forms the body admits.
    fn scan(
        &mut self,
        bytecode: &Bytecode,
        cost: &CostState,
        mut pc: usize,
        end: Option<usize>,
        mode: Mode,
    ) -> Option<usize> {
        let code = &bytecode.code;
        let fields = mode == Mode::Region;
        let leaf = mode == Mode::Leaf;
        while end.is_none_or(|end| pc < end) {
            let inst = *code.get(pc)?;
            let opcode = inst.opcode();
            self.join(pc)?;
            self.op_at.push((pc, self.ops.len()));
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
                    // `GET_UPVAL t u; GET_TABLE t t k`, both inside the body,
                    // with a key register other than `t`: canonical dispatch
                    // would read the key after the table replaced it.
                    Instr::OP_GET_UPVAL
                        if fields
                            && end.is_some_and(|end| pc + 1 < end)
                            && code.get(pc + 1).is_some_and(|index| {
                                index.opcode() == Instr::OP_GET_TABLE
                                    && index.a() == inst.a()
                                    && index.b() == inst.a()
                                    && index.c() != inst.a()
                            }) =>
                    {
                        let index = code[pc + 1];
                        // Both reads are free, so an exit before the pair
                        // owes no charge and none falls between its halves.
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_GET_TABLE).is_none());
                        }
                        let op = self.index(Receiver::Upvalue(inst.b()), pc, index)?;
                        self.ops.push(op);
                        self.charges.push((pc + 2, 0));
                        pc += 2;
                        continue;
                    }
                    // `GET_UPVAL t u; SELF r t k` with `t` one of `SELF`'s
                    // own destinations, so after the pair `t` holds nothing
                    // but what `SELF` wrote there.
                    Instr::OP_GET_UPVAL
                        if fields
                            && end.is_some_and(|end| pc + 1 < end)
                            && code.get(pc + 1).is_some_and(|lookup| {
                                lookup.opcode() == Instr::OP_SELF
                                    && lookup.b() == inst.a()
                                    && (lookup.a() == inst.a()
                                        || lookup.a().checked_add(1) == Some(inst.a()))
                            }) =>
                    {
                        let lookup = code[pc + 1];
                        // Both are free, so an exit before the pair owes no
                        // charge and none falls between its halves.
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_SELF).is_none());
                        }
                        let op = self.load(
                            lookup.a(),
                            Callee::Method {
                                receiver: Receiver::Upvalue(inst.b()),
                                key: lookup.c(),
                            },
                        )?;
                        self.ops.push(op);
                        self.charges.push((pc + 2, 0));
                        pc += 2;
                        continue;
                    }
                    // `SELF r s k` on a register the body never writes.
                    Instr::OP_SELF if fields => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_SELF).is_none());
                        }
                        if self.untouchable(inst.b()) {
                            return None;
                        }
                        self.receivers.push(inst.b());
                        self.load(
                            inst.a(),
                            Callee::Method {
                                receiver: Receiver::Register(inst.b()),
                                key: inst.c(),
                            },
                        )?
                    }
                    Instr::OP_GET_TABLE if fields => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_TABLE).is_none());
                        }
                        self.receivers.push(inst.b());
                        self.index(Receiver::Register(inst.b()), pc, inst)?
                    }
                    Instr::OP_BR_LT..=Instr::OP_BR_NE if fields => {
                        let compare = CompareOp::from_branch_opcode(opcode)?;
                        let b = self.read(inst.a())?;
                        let c = self.read(inst.b())?;
                        self.branch(pc, inst.sj(), end)?;
                        LoopOp {
                            kind: OpKind::Branch(compare),
                            a: 0,
                            b,
                            c,
                        }
                    }
                    Instr::OP_BR_LT_RK..=Instr::OP_BR_NE_RK if fields => {
                        let compare = CompareOp::from_branch_number_opcode(opcode)?;
                        let b = self.read(inst.a())?;
                        let c = self.number(bytecode, inst.b())?;
                        self.branch(pc, inst.sj(), end)?;
                        LoopOp {
                            kind: OpKind::Branch(compare),
                            a: 0,
                            b,
                            c,
                        }
                    }
                    Instr::OP_JMP if fields => {
                        self.branch(pc, inst.sj(), end)?;
                        self.dead = true;
                        LoopOp {
                            kind: OpKind::Jump,
                            a: 0,
                            b: 0,
                            c: 0,
                        }
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
                    Instr::OP_GET_GLOBAL if fields => self.load(
                        inst.a(),
                        Callee::Global {
                            name: inst.b(),
                            cache: inst.cache_slot(),
                        },
                    )?,
                    Instr::OP_GET_UPVAL if fields => {
                        self.load(inst.a(), Callee::Upvalue(inst.b()))?
                    }
                    Instr::OP_CALL if fields => self.call_site(inst, pc)?,
                    // A leaf's upvalue load defines a table receiver and
                    // writes no mirror slot; it is free, so it has no charge.
                    Instr::OP_GET_UPVAL if leaf => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                        }
                        let register = inst.a();
                        self.tables.retain(|&(r, _)| r != register);
                        self.tables.push((register, TableSource::Upvalue(inst.b())));
                        pc += 1;
                        continue;
                    }
                    Instr::OP_GET_FIELD if leaf => {
                        let source = self.receiver(inst.b())?;
                        let b = self.site(source, inst.c(), inst.cache_slot(), false)?;
                        LoopOp {
                            kind: OpKind::Read,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    Instr::OP_SET_FIELD if leaf => {
                        let source = self.receiver(inst.a())?;
                        let c = self.read(inst.c())?;
                        let b = self.site(source, inst.b(), inst.cache_slot(), true)?;
                        LoopOp {
                            kind: OpKind::Store,
                            a: 0,
                            b,
                            c,
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
        if end.is_some() {
            // The loop step: reached by fall-through and by branches to it.
            self.join(pc)?;
            self.op_at.push((pc, self.ops.len()));
        }
        // A branch into the second half of a fused pair was never joined.
        if !self.pending.is_empty() {
            return None;
        }
        Some(pc)
    }

    /// The finished body. `None` if a field receiver register is written by
    /// the body or is one of the first `reserved` registers (loop controls).
    fn finish(mut self, reserved: usize, tail: (usize, u64)) -> Option<(Body, Vec<u16>)> {
        if self.loading.is_some() {
            return None;
        }
        let control = !self.jumps.is_empty() || !self.indexed.is_empty();
        // Calls stay on the straight-line runner, whose nil-floor reasoning
        // assumes one path through the body; operation indices fit a `u8`.
        if control && (!self.calls.is_empty() || self.ops.len() > usize::from(u8::MAX)) {
            return None;
        }
        for &(op, target) in &self.jumps {
            let (_, index) = self.op_at.iter().find(|&&(pc, _)| pc == target)?;
            self.ops[op].a = *index as u8;
        }
        for &receiver in &self.receivers {
            if let Some(slot) = self.registers.iter().position(|&r| r == receiver)
                && (slot < reserved || self.written & (1 << slot) != 0)
            {
                return None;
            }
            if self.method_writes.contains(&receiver) {
                return None;
            }
        }
        if let (Some(floor), Some(from)) = (self.call_floor, self.nil_from) {
            // A call writes its function register and clears every register
            // above it (and the function register too when it keeps no
            // result), so a register read before written at or above the
            // lowest cleared one would read nil from the second pass on; the
            // loop controls and a receiver must survive every pass.
            let reserved_above = self.registers[..reserved].iter().any(|&r| r >= floor);
            let guard_above = self
                .registers
                .iter()
                .enumerate()
                .any(|(slot, &r)| self.guards & (1 << slot) != 0 && r >= from);
            if reserved_above || guard_above || self.receivers.iter().any(|&r| r >= from) {
                return None;
            }
            for site in &mut self.calls {
                let cleared = site.cleared();
                site.above = self
                    .registers
                    .iter()
                    .enumerate()
                    .filter(|&(_, &r)| r >= cleared)
                    .fold(0, |mask, (slot, _)| mask | 1 << slot);
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
                calls: self.calls.into_boxed_slice(),
                indexed: self.indexed.into_boxed_slice(),
                control,
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
        layout.scan(bytecode, cost, after, Some(step_pc), Mode::Region)?;
        let tail = Layout::charge(cost, Instr::OP_FOR_LOOP, step_pc);
        // The loop step reads the three hidden controls after the body,
        // guarded unless every path to it writes them.
        for slot in 0..3u8 {
            if layout.must & (1 << slot) == 0 {
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

/// A function whose code from pc 0 to its first `RETURN` is a numeric body,
/// possibly reading and storing fields of receiver tables, returning one
/// register or nothing, run by `State::try_numeric_leaf`.
#[derive(Debug)]
pub(super) struct NumericLeaf {
    body: Body,
    /// The returned register's mirror slot; `None` for `return` with no
    /// values.
    result: Option<u8>,
    /// The field sites the body reads and stores, in first-use order. A leaf
    /// with none and one result is pure: a typed loop region may call it.
    sites: Box<[FieldSite]>,
    /// The callee's frame size, which admission would reserve.
    max_stack: usize,
}

impl NumericLeaf {
    /// The leaf plan of `bytecode`, with fixed weights from `cost`. Every
    /// register the body reads before writing, as a number or as a field
    /// receiver, must be a parameter: any other is nil at admission, so
    /// canonical execution would fail or return nil, which the plan does not
    /// model.
    pub(super) fn recognize(bytecode: &Bytecode, cost: &CostState) -> Option<Self> {
        if bytecode.is_vararg {
            return None;
        }
        let mut layout = Layout::default();
        let ret_pc = layout.scan(bytecode, cost, 0, None, Mode::Leaf)?;
        let ret = *bytecode.code.get(ret_pc)?;
        if ret.opcode() != Instr::OP_RETURN {
            return None;
        }
        let result = match ret.return_count() {
            // The returned register is a read: `return x` must see a number.
            RetCount::Fixed(1) => Some(layout.read(ret.a())?),
            RetCount::Fixed(0) => None,
            _ => return None,
        };
        let params = u16::from(bytecode.num_params);
        let table_params_ok = layout.sites.iter().all(|site| match site.source {
            TableSource::Param(register) => register < params,
            TableSource::Upvalue(_) => true,
        });
        if !table_params_ok {
            return None;
        }
        let sites = std::mem::take(&mut layout.sites).into_boxed_slice();
        let tail = Layout::charge(cost, Instr::OP_RETURN, ret_pc);
        let (body, registers) = layout.finish(0, tail)?;
        for (slot, &register) in registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 && register >= params {
                return None;
            }
        }
        Some(Self {
            body,
            result,
            sites,
            max_stack: usize::from(bytecode.max_stack),
        })
    }
}

/// A loop region's call site resolved at entry: the callee, its runtime
/// (which holds the leaf plan), a method call's receiver and the leaf's
/// field sites resolved to live slots.
struct Resolved {
    callee: Val,
    runtime: Arc<BytecodeRuntime>,
    receiver: Val,
    /// A field leaf's sites; `None` for a leaf without any. Boxed so a
    /// region calling only leaves without fields keeps its resolved callees
    /// small.
    fields: Option<Box<FieldSlots>>,
}

impl Resolved {
    fn leaf(&self) -> &NumericLeaf {
        self.runtime
            .numeric_leaf
            .as_ref()
            .expect("resolve_callees found a leaf plan for every call site")
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
        if region.body.control {
            self.run_control_region(region, frame, ip, local_cost)
        } else if region.body.calls.is_empty() {
            self.run_region::<false>(region, frame, ip, local_cost)
        } else {
            self.run_region::<true>(region, frame, ip, local_cost)
        }
    }

    /// `run_numeric_loop` for a body with leaf calls (`CALLS`) or without,
    /// compiled apart so a body without calls keeps passes free of the call
    /// arms and the per-operation charge test.
    #[inline(always)]
    fn run_region<const CALLS: bool>(
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
        let mut callees: [Option<Resolved>; MAX_CALLS] = Default::default();
        if CALLS && !self.resolve_callees(body, frame, &mut callees) {
            return Ok(());
        }
        if CALLS
            && callees
                .iter()
                .flatten()
                .any(|resolved| resolved.fields.is_some())
        {
            self.run_passes::<CALLS, true>(region, frame, ip, local_cost, x, &callees)
        } else {
            self.run_passes::<CALLS, false>(region, frame, ip, local_cost, x, &callees)
        }
    }

    /// `run_region`'s passes, with the mirror loaded and the callees
    /// resolved; compiled apart for bodies calling field leaves (`FIELDS`),
    /// so a body calling only leaves without fields keeps its calls free of
    /// the field-site test.
    #[inline(always)]
    fn run_passes<const CALLS: bool, const FIELDS: bool>(
        &mut self,
        region: &NumericLoop,
        frame: &Frame,
        ip: &mut usize,
        local_cost: &mut u64,
        mut x: [f64; MIRROR_SLOTS],
        callees: &[Option<Resolved>; MAX_CALLS],
    ) -> Result<()> {
        let base = frame.stack_bottom;
        let body = &region.body;
        let leaf = |index: usize| {
            callees[index]
                .as_ref()
                .expect("resolve_callees resolved every call site")
                .leaf()
        };
        let weight = body
            .calls
            .iter()
            .enumerate()
            .fold(body.weight, |sum, (index, _)| {
                sum.saturating_add(leaf(index).body.weight)
            });

        let step_pc = region.step_pc;
        let mut written: u32 = 0;
        let mut published = false;
        // The call site whose callee is loaded and not yet called, which an
        // exit puts back in its register.
        let mut loaded: Option<usize> = None;
        let result = 'pass: loop {
            // Below the threshold no charge of the pass settles the batch,
            // so its sum, or the prefix before an exit, is added at once.
            let fast = weight == 0 || weight < self.cost_flush_at().saturating_sub(*local_cost);
            if !CALLS && fast {
                if weight != 0 {
                    *local_cost += weight;
                    #[cfg(feature = "cost-check-interval")]
                    body.record_charges();
                }
                for op in &body.ops {
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
            } else if !CALLS {
                for (op, &(cursor, charge)) in body.ops.iter().zip(&body.charges) {
                    if charge != 0 {
                        *ip = cursor;
                        if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
                            break 'pass Err(error);
                        }
                    }
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
                let (cursor, charge) = body.tail;
                if charge != 0 {
                    *ip = cursor;
                    if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
                        break Err(error);
                    }
                }
            } else {
                for (index, (op, &(cursor, charge))) in
                    body.ops.iter().zip(&body.charges).enumerate()
                {
                    if !fast && charge != 0 {
                        *ip = cursor;
                        if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
                            break 'pass Err(error);
                        }
                    }
                    match op.kind {
                        OpKind::Load => {
                            // The register holds the callee, not a number, until
                            // the call writes it.
                            let site = usize::from(op.b);
                            written &= !(1 << body.calls[site].slot);
                            loaded = Some(site);
                        }
                        OpKind::Call => {
                            let site = &body.calls[usize::from(op.b)];
                            let callee = leaf(usize::from(op.b));
                            // The call's admission safepoint, and in the
                            // per-operation path a callee charge that could be
                            // refused, hand the call to canonical dispatch.
                            if self.heap.gc_poll_requested()
                                || (!fast
                                    && !self.cost.admits_charges(*local_cost, callee.body.weight))
                            {
                                if fast {
                                    *local_cost += prefix_weight(body, &leaf, index);
                                    #[cfg(feature = "cost-check-interval")]
                                    record_pass(body, &leaf, index, false);
                                }
                                *ip = site.pc;
                                break 'pass Ok(());
                            }
                            // Leaf parameter `p` is argument `p`;
                            // `resolve_callees` checked every guarded one is
                            // a number passed.
                            let argument = |register: u16| x[at(site.args[usize::from(register)])];
                            let fields = if FIELDS {
                                callees[usize::from(op.b)]
                                    .as_ref()
                                    .and_then(|resolved| resolved.fields.as_deref())
                            } else {
                                None
                            };
                            let result = match fields {
                                None => callee.eval(argument),
                                Some(fields) => self.run_leaf_fields(callee, fields, argument),
                            };
                            if !fast {
                                for &(cursor, charge) in callee
                                    .body
                                    .charges
                                    .iter()
                                    .chain(std::iter::once(&callee.body.tail))
                                {
                                    if charge != 0
                                        && let Err(error) =
                                            self.charge_dispatch(local_cost, cursor, charge)
                                    {
                                        break 'pass Err(error);
                                    }
                                }
                            }
                            // Without a result `above` holds `r`'s own slot,
                            // so the write is dropped again.
                            x[at(op.a)] = result;
                            written = (written | 1 << op.a) & !site.above;
                            let cleared = base + usize::from(site.cleared());
                            self.stack[cleared..frame.top].fill(Val::Nil);
                            self.record_call_site(site.pc + 1);
                            loaded = None;
                        }
                        _ => {
                            x[at(op.a)] = eval(op, &x);
                            written |= 1 << op.a;
                        }
                    }
                }
                if fast {
                    if weight != 0 {
                        *local_cost += weight;
                        #[cfg(feature = "cost-check-interval")]
                        record_pass(body, &leaf, body.ops.len(), true);
                    }
                } else {
                    let (cursor, charge) = body.tail;
                    if charge != 0 {
                        *ip = cursor;
                        if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
                            break Err(error);
                        }
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
        // Last, so no published number overwrites it.
        if CALLS
            && let Some(site) = loaded
            && let Some(resolved) = &callees[site]
        {
            let function = base + usize::from(body.calls[site].register);
            self.stack[function] = resolved.callee;
            if body.calls[site].callee.method() {
                self.stack[function + 1] = resolved.receiver;
            }
        }
        result
    }

    /// `run_numeric_loop` for a body with forward branches or indexed reads,
    /// and no calls. The operations run by index, a branch or jump moving
    /// the index forward to its target.
    ///
    /// Beyond the straight-line runner's argument:
    /// - Each indexed read's receiver is read once at entry and must be a
    ///   table: it is an upvalue or a register the body never writes, and
    ///   nothing in the region stores, so the table and its contents stay
    ///   fixed. Each read is a raw get with the number key, as `GET_TABLE`
    ///   makes first; a number result is the canonical result even under a
    ///   metatable, which only a raw nil consults. Anything else publishes
    ///   the mirror and exits with the cursor on the read's first
    ///   instruction, both of whose instructions are free, so canonical
    ///   dispatch makes the read with its full semantics.
    /// - A slot first written on a later pass is published before that
    ///   pass's checkpoint (`published`), so every checkpoint sees numbers
    ///   where canonical execution would have overwritten the old values.
    /// - The pass bound is the sum of every operation's weight, which
    ///   bounds any path, so below the flush threshold each executed
    ///   operation's weight is added with no settlement test.
    #[inline(never)]
    fn run_control_region(
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
        let mut tables = [None::<ObjectPtr>; MAX_INDEXED];
        for (table, &(receiver, _)) in tables.iter_mut().zip(&body.indexed) {
            let value = match receiver {
                Receiver::Upvalue(index) => self.upvalue_value(frame, index),
                Receiver::Register(register) => self.stack[base + usize::from(register)],
            };
            let Val::Obj(ptr) = value else {
                return Ok(());
            };
            if self.heap.as_table_ref(ptr).is_none() {
                return Ok(());
            }
            *table = Some(ptr);
        }
        let step_pc = region.step_pc;
        let mut written: u32 = 0;
        let mut published: u32 = 0;
        let result = 'pass: loop {
            let fast =
                body.weight == 0 || body.weight < self.cost_flush_at().saturating_sub(*local_cost);
            let mut k = 0;
            while let Some(op) = body.ops.get(k) {
                let (cursor, charge) = body.charges[k];
                if charge != 0 {
                    if fast {
                        *local_cost += charge;
                        #[cfg(feature = "cost-check-interval")]
                        crate::cost_meter::interval_test::record(cursor, charge);
                    } else {
                        *ip = cursor;
                        if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
                            break 'pass Err(error);
                        }
                    }
                }
                k += 1;
                match op.kind {
                    OpKind::Branch(compare) => {
                        if !holds(compare, x[at(op.b)], x[at(op.c)]) {
                            k = usize::from(op.a);
                        }
                    }
                    OpKind::Jump => k = usize::from(op.a),
                    OpKind::Index => {
                        let (_, exit) = body.indexed[usize::from(op.b)];
                        let table = tables[usize::from(op.b) % MAX_INDEXED]
                            .expect("every indexed receiver was resolved at entry");
                        let value = self
                            .heap
                            .as_table_ref(table)
                            .map(|tbl| tbl.get(&Val::Num(x[at(op.c)])));
                        let Some(Val::Num(n)) = value else {
                            *ip = exit;
                            break 'pass Ok(());
                        };
                        x[at(op.a)] = n;
                        written |= 1 << op.a;
                    }
                    _ => {
                        x[at(op.a)] = eval(op, &x);
                        written |= 1 << op.a;
                    }
                }
            }
            let (cursor, charge) = body.tail;
            if charge != 0 {
                if fast {
                    *local_cost += charge;
                    #[cfg(feature = "cost-check-interval")]
                    crate::cost_meter::interval_test::record(cursor, charge);
                } else {
                    *ip = cursor;
                    if let Err(error) = self.charge_dispatch(local_cost, cursor, charge) {
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
            let fresh = written & !published;
            if fresh != 0 {
                self.publish_mirror(body, base, &x, fresh);
                published |= fresh;
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

    /// Read every call site's callee into `callees` and check what the
    /// canonical admission of each of its calls would check that the region
    /// cannot change: the callee is a Lua closure with a leaf plan whose
    /// parameter reads are all number arguments, the call depth, the frame
    /// reservation under the stack cap, and no open upvalue in a call window.
    /// The stack must also end at the frame's top, where a fixed-result call
    /// leaves it. A call keeping a result needs a leaf that returns one.
    /// `false` declines the region.
    ///
    /// A method call's receiver must be a table and its method is found
    /// through `__index` tables only (`State::region_method`). A field
    /// leaf's sites are resolved here, once (`State::resolve_sites`): its
    /// receiver sources are the method receiver and the callee's upvalues.
    /// An upvalue open on this frame's registers, which the region writes,
    /// declines; one open on an enclosing frame's names a slot nothing in
    /// the region writes.
    ///
    /// Read once: the region and its leaves store only numbers, only into
    /// slots already live, under keys that are not `__index` (checked by
    /// `resolve_sites`) and, checked here, that are neither a method key, a
    /// callee's global name nor the key of a field the region reads at entry.
    /// No key is created or removed and no table version moves, so every
    /// lookup made here (a global, an upvalue, a receiver, each raw miss,
    /// `__index` and metatable link of a method route, each field slot and
    /// each read-once field) answers the same on every pass, and a
    /// collection changes no live table. The bindings keep the callees and
    /// receivers reachable.
    fn resolve_callees(
        &self,
        body: &Body,
        frame: &Frame,
        callees: &mut [Option<Resolved>; MAX_CALLS],
    ) -> bool {
        let base = frame.stack_bottom;
        if self.stack.len() != frame.top || self.in_flight.call_depth >= MAX_CALL_DEPTH {
            return false;
        }
        for (index, site) in body.calls.iter().enumerate() {
            let function = base + usize::from(site.register);
            if self
                .in_flight
                .open_upvalues
                .last()
                .is_some_and(|&(open, _)| open >= function)
            {
                return false;
            }
            let (value, receiver) = match site.callee {
                Callee::Global { name, cache } => (self.read_global(frame, name, cache), Val::Nil),
                Callee::Upvalue(index) => (self.upvalue_value(frame, index), Val::Nil),
                Callee::Method { receiver, key } => {
                    let receiver = match receiver {
                        Receiver::Upvalue(index) => self.upvalue_value(frame, index),
                        Receiver::Register(register) => self.stack[base + usize::from(register)],
                    };
                    let Some(method) = self.region_method(receiver, frame.literal(key)) else {
                        return false;
                    };
                    (method, receiver)
                }
            };
            let Val::Obj(ptr) = value else {
                return false;
            };
            let Some(closure) = self.heap.as_lua_function_ref(ptr) else {
                return false;
            };
            let Some(leaf) = closure.runtime.numeric_leaf.as_ref() else {
                return false;
            };
            if site.results == 1 && leaf.result.is_none() {
                return false;
            }
            // Parameter 0 of a method call is the receiver, a table.
            let skip = usize::from(site.callee.method());
            let arguments = site.args.len();
            let parameters_are_arguments =
                leaf.body
                    .registers
                    .iter()
                    .enumerate()
                    .all(|(slot, &register)| {
                        let register = usize::from(register);
                        leaf.body.guards & (1 << slot) == 0
                            || (register >= skip && register < arguments)
                    });
            if !parameters_are_arguments
                || function + 1 + leaf.max_stack + FRAME_HEADROOM > MAX_STACK_SIZE
            {
                return false;
            }
            let fields = if leaf.sites.is_empty() {
                None
            } else {
                let mut fields = Box::new([None; MAX_FIELD_SITES]);
                let param = |register: u16| (skip == 1 && register == 0).then_some(receiver);
                if !self.resolve_sites(closure, leaf, param, base, &mut fields) {
                    return false;
                }
                Some(fields)
            };
            callees[index] = Some(Resolved {
                callee: value,
                runtime: Arc::clone(&closure.runtime),
                receiver,
                fields,
            });
        }
        self.region_keys_disjoint(body, frame, callees)
    }

    /// Whether no leaf of `callees` stores under a key some resolution of
    /// the region relies on: a method key, a callee's global name or a field
    /// the region reads at entry. Keys are interned, so equal keys are equal
    /// values.
    fn region_keys_disjoint(
        &self,
        body: &Body,
        frame: &Frame,
        callees: &[Option<Resolved>; MAX_CALLS],
    ) -> bool {
        let read_keys = body
            .calls
            .iter()
            .filter_map(|site| match site.callee {
                Callee::Global { name, .. } => Some(name),
                Callee::Method { key, .. } => Some(key),
                Callee::Upvalue(_) => None,
            })
            .chain(
                body.constants
                    .iter()
                    .filter_map(|&(_, constant)| match constant {
                        Constant::Field { key, .. } => Some(key),
                        Constant::Number(_) => None,
                    }),
            );
        // bounded: at most MAX_CALLS sites of MAX_FIELD_SITES each, against
        // the body's own sites and constants
        let mut stored = callees.iter().flatten().flat_map(|resolved| {
            let runtime = &resolved.runtime;
            resolved
                .leaf()
                .sites
                .iter()
                .filter(|site| site.store)
                .map(|site| runtime.literals[usize::from(site.key)])
        });
        let read_keys: Vec<Val> = read_keys.map(|key| frame.literal(key)).collect();
        stored.all(|key| !read_keys.contains(&key))
    }

    /// The method `key` of `receiver` as `SELF` would find it, when that
    /// lookup runs nothing and cannot fail: `receiver` is a table, and each
    /// raw miss continues through a metatable whose `__index` is a table,
    /// until a raw hit. Every continuing step past the receiver's own lookup
    /// takes a metamethod depth unit (`State::resolve_index`), so the walk
    /// must fit under the limit from the depth in flight. `None` declines:
    /// any other handler, a nil answer, or too deep a walk.
    fn region_method(&self, receiver: Val, key: Val) -> Option<Val> {
        let index_key = self.meta_names.get(MetaName::Index);
        let Val::Obj(mut table) = receiver else {
            return None;
        };
        let mut depth = self.in_flight.metamethod_depth;
        let mut first = true;
        // bounded: each continuing step past the first takes a depth unit,
        // and the walk stops at MAX_METAMETHOD_DEPTH
        loop {
            let tbl = self.heap.as_table_ref(table)?;
            let value = tbl.get(&key);
            if !matches!(value, Val::Nil) {
                return Some(value);
            }
            let metatable = self.heap.as_table_ref(tbl.get_metatable()?)?;
            let Val::Obj(next) = metatable.get(&index_key) else {
                return None;
            };
            if !first {
                if depth >= MAX_METAMETHOD_DEPTH {
                    return None;
                }
                depth += 1;
            }
            first = false;
            table = next;
        }
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
    /// - every field site resolves to a live slot of a table, holding a
    ///   number where the body reads it (`State::resolve_sites`), so no read
    ///   or store consults a metamethod or fails;
    /// - the callee's charges, body then `RETURN`, sum strictly below the
    ///   flush threshold minus the pending batch, so adding them at once
    ///   leaves the batch where per-instruction charging would.
    ///
    /// Receivers are tracked by the reaching definition at each use, so a
    /// register that held a table and then a number is never taken as the
    /// table. The body runs no call, allocation, capture or upvalue write,
    /// and stores only numbers into slots already live, so nothing can
    /// observe the missing activation: no traceback, call-site record,
    /// collection or handler runs while it would exist, and the next
    /// checkpoint sees the roots canonical execution would leave. The result
    /// lands in the function slot and the stack is cut above it, as the
    /// return slides it; a return with no values cuts it at the function
    /// slot. The `OP_CALL` arm's resize then supplies a fixed count's
    /// missing results, as after a return.
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
        let (result, weight) = if leaf.sites.is_empty() {
            for op in &body.ops {
                x[at(op.a)] = eval(op, &x);
            }
            #[cfg(feature = "cost-check-interval")]
            if body.weight != 0 {
                body.record_charges();
            }
            (leaf.result.map(|result| x[at(result)]), body.weight)
        } else {
            let mut fields = [None; MAX_FIELD_SITES];
            let param = |register: u16| {
                (register < u16::from(num_args))
                    .then(|| self.stack[idx + 1 + usize::from(register)])
            };
            if !self.resolve_sites(closure, leaf, param, usize::MAX, &mut fields) {
                return false;
            }
            // The stores need the heap mutably, so hold the plan through its
            // runtime rather than through the closure.
            let runtime = Arc::clone(&closure.runtime);
            let leaf = runtime
                .numeric_leaf
                .as_ref()
                .expect("the closure's runtime holds the plan just read");
            self.run_fields(&leaf.body, &fields, &mut x);
            #[cfg(feature = "cost-check-interval")]
            if leaf.body.weight != 0 {
                leaf.body.record_charges();
            }
            (leaf.result.map(|result| x[at(result)]), leaf.body.weight)
        };
        *local_cost += weight;
        match result {
            Some(result) if expected != RetCount::Fixed(0) => {
                self.stack[idx] = Val::Num(result);
                self.stack.truncate(idx + 1);
            }
            _ => self.stack.truncate(idx),
        }
        true
    }

    /// Resolve every field site of field leaf `leaf`, called through
    /// `closure` with `param(p)` the value passed as parameter `p` (`None`
    /// when not passed), to the live slot of its receiver table, into
    /// `fields`. `false`, changing nothing a later lookup could answer
    /// differently, when the call must run canonically: a receiver is not a
    /// table, a key is not raw present in it, a read field does not hold a
    /// number, a stored key is `__index`, or an upvalue source is open on a
    /// stack slot at or above `open_below`.
    ///
    /// Why that makes the body exact: a store writes a number into a slot
    /// already live, which creates or removes no key, moves no slot and no
    /// table version, so every site checked here stays present, and every
    /// read site keeps holding a number, through the whole body, even when
    /// two sources name one table. Raw presence means canonical execution
    /// consults no `__index` or `__newindex` either. The cache entries this
    /// consults or refills only accelerate the lookup, as dispatch's do.
    fn resolve_sites(
        &self,
        closure: &super::object::Closure,
        leaf: &NumericLeaf,
        param: impl Fn(u16) -> Option<Val>,
        open_below: usize,
        fields: &mut [Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
    ) -> bool {
        // The inline-cache differential compares leaves with the canonical
        // path, as library leaves do (`vm/leaf.rs`).
        #[cfg(feature = "disable-inline-caches")]
        if super::caches::hits_disabled() {
            return false;
        }
        let runtime = &closure.runtime;
        let index_key = self.meta_names.get(MetaName::Index);
        for (site, field) in leaf.sites.iter().zip(fields.iter_mut()) {
            let receiver = match site.source {
                TableSource::Param(register) => {
                    let Some(value) = param(register) else {
                        return false;
                    };
                    value
                }
                TableSource::Upvalue(index) => {
                    let Some(&uv_ref) = closure.upvalues.as_slice().get(usize::from(index)) else {
                        return false;
                    };
                    match self.upvalue_pool.get(uv_ref) {
                        Upvalue::Open(stack_idx) if *stack_idx < open_below => {
                            self.stack[*stack_idx]
                        }
                        Upvalue::Open(_) => return false,
                        Upvalue::Closed(value) => *value,
                    }
                }
            };
            let Val::Obj(table) = receiver else {
                return false;
            };
            let Some(tbl) = self.heap.as_table_ref(table) else {
                return false;
            };
            let key = runtime.literals[usize::from(site.key)];
            if site.store && key == index_key {
                return false;
            }
            let cached = site
                .read_cache
                .and_then(|slot| runtime.caches.field(Some(slot)))
                .and_then(FieldLookupCacheSlot::get_field)
                .or_else(|| {
                    site.set_cache
                        .and_then(|slot| runtime.caches.set_field(Some(slot)))
                        .and_then(SetFieldLookupCacheSlot::get)
                })
                .filter(|entry| entry.table == table)
                .and_then(|entry| Some((entry.slot, tbl.read_slot(entry.slot)?)));
            let (slot, value) = match cached {
                Some(hit) => hit,
                None => {
                    let Some((slot, value)) = tbl.find_slot(&key) else {
                        return false;
                    };
                    let entry = FieldLookupCacheEntry { table, slot };
                    if let Some(cache) = site.read_cache.and_then(|s| runtime.caches.field(Some(s)))
                    {
                        cache.set_field(entry);
                    }
                    if let Some(cache) = site
                        .set_cache
                        .and_then(|s| runtime.caches.set_field(Some(s)))
                    {
                        cache.set(entry);
                    }
                    (slot, value)
                }
            };
            if site.number && !matches!(value, Val::Num(_)) {
                return false;
            }
            *field = Some((table, slot));
        }
        true
    }

    /// A loop region's call of field leaf `leaf` with its sites resolved to
    /// `fields` and `argument(p)` as each number parameter it reads; its
    /// result, or 0 when it returns nothing (a call keeping a result
    /// declines such a leaf at entry).
    #[inline(never)]
    fn run_leaf_fields(
        &mut self,
        leaf: &NumericLeaf,
        fields: &[Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
        argument: impl Fn(u16) -> f64,
    ) -> f64 {
        let body = &leaf.body;
        let mut y = [0.0f64; MIRROR_SLOTS];
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                y[slot] = argument(register);
            }
        }
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                y[at(slot)] = value;
            }
        }
        self.run_fields(body, fields, &mut y);
        leaf.result.map_or(0.0, |result| y[at(result)])
    }

    /// Run a field leaf's body over `x` with its sites resolved to
    /// `fields`, in program order: each read reloads its slot, each store
    /// overwrites it with a number.
    fn run_fields(
        &mut self,
        body: &Body,
        fields: &[Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
        x: &mut [f64; MIRROR_SLOTS],
    ) {
        for op in &body.ops {
            match op.kind {
                OpKind::Read => {
                    let (table, slot) =
                        fields[usize::from(op.b)].expect("resolve_sites resolved every site");
                    let value = self
                        .heap
                        .as_table_ref(table)
                        .and_then(|tbl| tbl.read_slot(slot));
                    let Some(Val::Num(n)) = value else {
                        panic!(
                            "a read site resolved to a number stays one: stores write numbers into live slots"
                        );
                    };
                    x[at(op.a)] = n;
                }
                OpKind::Store => {
                    let (table, slot) =
                        fields[usize::from(op.b)].expect("resolve_sites resolved every site");
                    // A live overwrite under a key proven not `__index`, so
                    // inherited routes stay standing (`GcHeap::overwrite_live_slot`).
                    let stored = self
                        .heap
                        .overwrite_live_slot(table, slot, Val::Num(x[at(op.c)]));
                    debug_assert!(stored, "a resolved site stays live");
                }
                _ => x[at(op.a)] = eval(op, x),
            }
        }
    }
}

/// A compare-branch's comparison of two numbers, as dispatch makes it:
/// IEEE, so every ordering and `==` are false on NaN and `~=` true.
#[inline(always)]
#[expect(
    clippy::float_cmp,
    reason = "Lua `==` on numbers is exact IEEE equality"
)]
fn holds(compare: CompareOp, x: f64, y: f64) -> bool {
    match compare {
        CompareOp::Lt => x < y,
        CompareOp::Le => x <= y,
        CompareOp::Gt => x > y,
        CompareOp::Ge => x >= y,
        CompareOp::Eq => x == y,
        CompareOp::Ne => x != y,
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
        // Calls and loads are run by the region loop, and field reads and
        // stores by a field leaf, never through here.
        // Branches, jumps and indexed reads only by `run_control_region`.
        OpKind::Copy
        | OpKind::Load
        | OpKind::Call
        | OpKind::Read
        | OpKind::Store
        | OpKind::Branch(_)
        | OpKind::Jump
        | OpKind::Index => lhs,
    }
}

impl NumericLeaf {
    /// The leaf's result with `argument(register)` as each parameter it
    /// reads, which the caller has checked is a number it passes.
    #[inline(always)]
    fn eval(&self, argument: impl Fn(u16) -> f64) -> f64 {
        let body = &self.body;
        let mut y = [0.0f64; MIRROR_SLOTS];
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                y[slot] = argument(register);
            }
        }
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                y[at(slot)] = value;
            }
        }
        for op in &body.ops {
            y[at(op.a)] = eval(op, &y);
        }
        // A call keeping a result declines a leaf that returns nothing.
        self.result.map_or(0.0, |result| y[at(result)])
    }
}

/// The charges of a pass's operations before operation `end`, the leaves
/// of the calls among them included.
fn prefix_weight<'a>(body: &Body, leaf: &impl Fn(usize) -> &'a NumericLeaf, end: usize) -> u64 {
    let ops = body.charges[..end].iter().map(|&(_, weight)| weight);
    let leaves = body
        .calls
        .iter()
        .enumerate()
        .filter(|(_, site)| site.op < end)
        .map(|(index, _)| leaf(index).body.weight);
    ops.chain(leaves).fold(0, u64::saturating_add)
}

/// Record the positive charges of a pass's operations before `end`, each
/// call's leaf charges after the call's own, then `FOR_LOOP`'s if `tail`,
/// in the charge-site trace, as per-instruction dispatch would.
#[cfg(feature = "cost-check-interval")]
fn record_pass<'a>(body: &Body, leaf: &impl Fn(usize) -> &'a NumericLeaf, end: usize, tail: bool) {
    for (op, &(cursor, weight)) in body.ops.iter().zip(&body.charges).take(end) {
        if weight != 0 {
            crate::cost_meter::interval_test::record(cursor, weight);
        }
        if let OpKind::Call = op.kind {
            leaf(usize::from(op.b)).body.record_charges();
        }
    }
    if tail && body.tail.1 != 0 {
        crate::cost_meter::interval_test::record(body.tail.0, body.tail.1);
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
    fn regions_take_calls_with_one_result_and_no_nil_reads() {
        // A global or upvalue callee, or a method of a receiver the body
        // never writes, called with fixed arguments for zero or one result.
        // Calls are off under fault injection.
        let taken = usize::from(!cfg!(feature = "fault-injection"));
        for source in [
            "local function f() local s = 0 for i = 1, 9 do s = g(s, i) end end",
            "local h local function f() local s = 0 for i = 1, 9 do s = h(s) + 1 end end",
            "local function f() local s = 0 for i = 1, 9 do g(s) end end",
            "local function f(o) local s = 0 for i = 1, 9 do s = o:m(s) end end",
            "local o local function f() for i = 1, 9 do o:m(i) end end",
            "local o local function f() local s = 0 for i = 1, 9 do s = s + o:m(i, 2) end end",
        ] {
            assert_eq!(regions(source), taken, "{source}");
        }
        // A nested call has two callees loaded at once; open arguments are
        // refused; a method receiver must not change.
        for source in [
            "local function f() local s = 0 for i = 1, 9 do s = g(h(s)) end end",
            "local function f(...) local s = 0 for i = 1, 9 do s = g(...) end end",
            "local function f(o) local s = 0 for i = 1, 9 do s = o:m(s); o = i end end",
            // A callee loaded and never called holds a non-number.
            "local function f() local s = 0 for i = 1, 9 do s = s + 1 local _ = g end end",
        ] {
            assert_eq!(regions(source), 0, "{source}");
        }
    }

    #[test]
    fn regions_take_forward_branches_and_indexed_reads() {
        for source in [
            "local t = {} local function f() local s = 0 for i = 1, 9 do if t[i] < 3 or t[i] == 2 then s = s + 1 end end end",
            "local function f(t) local s = 0 for i = 1, 9 do local v = t[i] if v > s then s = v else s = s - 1 end end end",
            "local function f() local s = 0 for i = 1, 9 do if s > 4 then s = 0 end s = s + i end end",
        ] {
            assert_eq!(regions(source), 1, "{source}");
        }
        for source in [
            // A receiver the body writes; a branch beside a call.
            "local function f(t) local s = 0 for i = 1, 9 do s = s + t[i]; t = i end end",
            "local function f() local s = 0 for i = 1, 9 do if s > 1 then s = g(s) end end end",
            // A string comparison and a truth test are not number branches.
            "local function f(t) local s = 0 for i = 1, 9 do if s == 'x' then s = 1 end end end",
            "local function f(t) local s = 0 for i = 1, 9 do if s then s = 1 end end end",
        ] {
            assert_eq!(regions(source), 0, "{source}");
        }
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
        // Field reads and stores on parameter and upvalue receivers, and a
        // return with no values.
        assert!(leaf(
            "local function f(self, n) self.count = self.count + n return self.count end"
        ));
        assert!(leaf("local function f(self, n) self.hp = self.hp - n end"));
        assert!(leaf(
            "local s = {} local function f(o) s.c = s.c + 1 return o.w * o.h end"
        ));
        assert!(leaf("local function f() end"));
        for source in [
            // A receiver used as a number, stored, or returned.
            "local function f(t) t.x = t.x + t end",
            "local function f(t) t.x = t end",
            "local function f(t) t.x = 1 return t end",
            // A receiver register later holding a number is no receiver.
            "local s = {} local function f() local a = s; a = 1; a.x = 2 end",
            // A receiver that is not a parameter.
            "local function f() local t; t.x = 1 end",
        ] {
            assert!(!leaf(source), "{source}");
        }
        // 31 parameters and one temporary fill the mirror with no constants.
        let params = (0..31).map(|i| format!("p{i}")).collect::<Vec<_>>();
        let sum = params.join(" + ");
        assert!(leaf(&format!(
            "local function f({}) return {sum} end",
            params.join(", ")
        )));
    }
}
