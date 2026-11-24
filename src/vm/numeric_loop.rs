//! Straight-line number arithmetic runs in `f64` locals in two shapes:
//!
//! - **Typed loop regions.** A numeric `for` loop whose body is such
//!   arithmetic runs as one region: the registers the loop touches live in an
//!   `f64` mirror, and each iteration runs its body operations and the loop
//!   step without going back through the dispatch loop. A body may also
//!   branch forward on number comparisons, read elements of loop-invariant
//!   tables, read a record (a table element) per iteration whose number
//!   fields it reads and stores, call a method of that record, and hold
//!   nested numeric loops (`State::run_control_region`).
//! - **Numeric leaves.** A Lua function whose code up to its first `RETURN`
//!   is such arithmetic, returning one value or none, runs inside call
//!   dispatch without an activation when its arguments are numbers. Its
//!   body may also read and store string-keyed fields of receiver tables (a
//!   parameter used only as a receiver, or an upvalue), the method shape
//!   `self.count = self.count + n`; such a field leaf runs when every field
//!   is raw present in its table. A leaf may branch forward, on number
//!   comparisons and on a field's truthiness (`if self.alive then`), as
//!   long as every edge reaches that first `RETURN`; it then charges the
//!   path it ran, bounded at admission by the sum over every path. A loop
//!   region may call either kind,
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
//!   number into a slot already live (a field leaf's, or a record's). The
//!   only foreign code is the back-edge checkpoint
//!   (`State::poll_gc_checkpoint`), which polls the interrupt and may
//!   collect, and runs no Lua or host code. A guard at entry on every
//!   register read before it is written in one pass of the body and the loop
//!   step therefore proves every read of the whole loop is a number. The
//!   guard reads operands before the destination is marked written, so
//!   `R0 = R0 + k` guards `R0`. The recognizer keeps per-register facts
//!   (written on every path, not cleared by a call, holding a record) over
//!   forward edges, and meets each loop head's facts with those its back
//!   edges carry, scanning again until no head narrows
//!   (`NumericLoop::recognize`), so a read a later iteration makes is
//!   proven as the first's.
//! - Field reads. `GET_UPVAL r u; GET_FIELD r r k` (the scratch register
//!   reused as the destination) and `GET_FIELD d s k` with `s` a register
//!   the body never writes read a receiver that cannot change while the
//!   region runs: nothing in it writes an upvalue, a register outside the
//!   mirror or a table field under the read's key (a called field leaf's
//!   and a record's store keys are checked against it at entry, and a
//!   record never replaces a receiver register), a running closure's
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
//!   the charges of the operations before the exit only. A method of a
//!   record read this pass is looked up on every pass through a cache of
//!   the run, keyed by the record's metatable (`State::run_control_region`
//!   says why each entry stays exact).
//! - A `GET_UPVAL r u` whose register is next read as a number is a number
//!   read once at entry, as a field read is.
//! - The loop step reads the mirror's controls, which a body may have
//!   written, computes the next value into a temporary, polls on the taken
//!   back edge with the cursor past `FOR_LOOP`, and only then writes the
//!   hidden and visible counters, so an interrupted iteration writes no
//!   counter. A nested loop's `FOR_PREP` and `FOR_LOOP` run the same way
//!   inside the body, its taken back edge polling with the cursor past its
//!   own `FOR_LOOP`.
//!
//! Why a numeric leaf is exact: see `State::try_numeric_leaf`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
    /// Entries left to decline after a run that met a method it could not
    /// call on its first pass (`State::run_control_region`): a loop whose
    /// record methods are no leaves would otherwise pay the entry and that
    /// first pass on every entry, for nothing. Declining runs the loop
    /// canonically, which is exact, so this only changes the time taken.
    /// Atomic only so the runtime stays `Sync`; one State's execution is the
    /// only reader and writer.
    backoff: AtomicU8,
}

/// Entries a region declines after its first pass met a method it could
/// not call.
const BACKOFF_ENTRIES: u8 = 16;

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
    /// A loop's record registers, by record id: the frame register and its
    /// mirror slot when the body also writes it as a number. Empty for a
    /// leaf.
    records: Box<[(u16, Option<u8>)]>,
    /// A loop's record field sites, read or stored per iteration on the
    /// table its record register holds. Empty for a leaf.
    record_sites: Box<[RecordSite]>,
    /// A loop's nested numeric loops, by `Prep` order. Empty for a leaf.
    inner: Box<[InnerLoop]>,
    /// The body has forward branches, indexed reads, records, nested loops
    /// or method calls on a record, so it runs through
    /// `State::run_control_region` rather than as a straight line.
    control: bool,
}

/// A numeric `for` nested in a loop body: its `FOR_PREP` and `FOR_LOOP`
/// run as the region's `Prep` and `Step` operations over the same mirror.
#[derive(Clone, Copy, Debug)]
struct InnerLoop {
    /// Mirror slots of its four controls (start/counter, limit, step,
    /// visible variable).
    controls: [u8; 4],
    /// The inner `FOR_LOOP` pc.
    step_pc: usize,
    /// The operation index of the first body operation.
    body: usize,
    /// The operation index of the instruction after the inner `FOR_LOOP`,
    /// where a `Prep` that does not enter goes.
    exit: usize,
}

/// Loops one region may nest inside its own.
const MAX_INNER: usize = 8;

/// Nesting depth of loops inside one region.
const MAX_DEPTH: usize = 4;

/// Scans of one region while its loop-head facts descend to a fixpoint.
const MAX_SCANS: usize = 8;

/// Classes one method-call site on a record may resolve per region run.
const MAX_METHOD_CLASSES: usize = 8;

/// Register facts at one program point, keyed by fact index (one per
/// register the region names, fixed before the solving scans).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Facts {
    /// Written on every path since the region's entry: a read outside it
    /// is guarded at entry.
    must: u64,
    /// Not cleared by a call on any path since its last write.
    safe: u64,
    /// Holds a record defined in this pass on every path.
    rmust: u64,
    /// May hold a record on some path.
    rmay: u64,
}

impl Facts {
    /// The identity of `meet`, for a point no edge has reached yet.
    const TOP: Self = Facts {
        must: u64::MAX,
        safe: u64::MAX,
        rmust: u64::MAX,
        rmay: 0,
    };
    /// The region's entry: nothing written, nothing cleared, no record.
    const ENTRY: Self = Facts {
        must: 0,
        safe: u64::MAX,
        rmust: 0,
        rmay: 0,
    };

    fn meet(self, other: Self) -> Self {
        // An intersection of facts, a union of possibilities.
        Facts {
            must: self.must & other.must,
            safe: self.safe & other.safe,
            rmust: self.rmust & other.rmust,
            rmay: self.rmay | other.rmay,
        }
    }

    /// After a number write of fact `fact`.
    fn wrote(self, fact: usize) -> Self {
        let bit = 1u64 << fact;
        Facts {
            must: self.must | bit,
            safe: self.safe | bit,
            rmust: self.rmust & !bit,
            rmay: self.rmay & !bit,
        }
    }
}

impl Default for Facts {
    fn default() -> Self {
        Facts::ENTRY
    }
}

/// A loop being scanned inside a region: where its back edge goes and the
/// facts its head assumed.
#[derive(Clone, Copy)]
struct OpenLoop {
    index: usize,
    base: u16,
    step_pc: usize,
    head: usize,
    entry: Facts,
}

/// Indexed reads one loop body may make.
const MAX_INDEXED: usize = 8;

/// Record registers one loop body may define.
const MAX_RECORDS: usize = 8;

/// Record field sites one loop body may touch.
const MAX_RECORD_SITES: usize = 16;

/// A field of a loop's record: the record id and the key literal, shared
/// by every `GET_FIELD` and `SET_FIELD` of the body naming both.
#[derive(Clone, Copy, Debug)]
struct RecordSite {
    record: u8,
    key: u16,
    /// Some constituent stores to it.
    store: bool,
}

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
    /// The callee load's pc: a method lookup on a record exits there.
    load_pc: usize,
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
    /// `SELF r t k` on record register `t`: method `k` of the record this
    /// pass read, resolved per pass through a cache keyed by the record's
    /// metatable (`State::run_control_region`).
    Dynamic {
        record: u8,
        key: u16,
    },
}

impl Callee {
    fn method(self) -> bool {
        matches!(self, Callee::Method { .. } | Callee::Dynamic { .. })
    }

    fn dynamic(self) -> bool {
        matches!(self, Callee::Dynamic { .. })
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
    /// A number upvalue of the running closure, read at entry: nothing a
    /// region runs writes an upvalue, and a running closure's upvalues
    /// never alias its own frame's registers.
    Upvalue(u16),
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
    /// Some constituent reads it as a number, so it must hold one at entry.
    /// A site only truth-tested may hold any value, but must be raw present
    /// as every site must.
    number: bool,
    /// Some constituent stores to it, so its key must not be `__index`.
    store: bool,
}

/// How a leaf constituent uses its field site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SiteUse {
    /// `GET_FIELD` read as a number.
    Number,
    /// `GET_FIELD` read only for its truthiness.
    Truth,
    /// `SET_FIELD` of a number.
    Store,
}

/// Operations a leaf with forward branches may hold: its runners report
/// the operations a call ran as one bit each.
const MAX_CONTROL_LEAF_OPS: usize = 64;

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
    /// A field leaf's `GET_FIELD d s k; TEST_FALSE d` (`false`) or
    /// `TEST_TRUE d` (`true`): when the live value of field site `b` has
    /// the given truthiness, continue at operation index `a`. Writes no
    /// mirror slot; `d` is left unreadable (`Layout::truth_test`).
    Test(bool),
    /// A loop's forward compare-branch: when `b` compared with `c` is
    /// false, continue at operation index `a`. Writes no mirror slot.
    Branch(CompareOp),
    /// A loop's forward `JMP` to operation index `a`. Writes no mirror slot.
    Jump,
    /// A loop's raw read of indexed receiver `b` at the key in slot `c`
    /// into slot `a`, exiting to canonical dispatch unless it is a number.
    Index,
    /// A loop's raw read of indexed receiver `b` at the key in slot `c`
    /// into record `a`, exiting to canonical dispatch unless it is a table.
    /// Writes no mirror slot.
    Record,
    /// A loop's raw read of record site `b` into slot `a`, exiting to
    /// canonical dispatch unless it is a live number.
    RecordRead,
    /// A loop's store of slot `c` into the live slot of record site `b`,
    /// exiting to canonical dispatch before its charge when the key is not
    /// live there. Writes no mirror slot.
    RecordStore,
    /// A nested loop's `FOR_PREP`, of inner loop `b`.
    Prep,
    /// A nested loop's `FOR_LOOP`, of inner loop `b`: a taken back edge
    /// polls and continues at the inner body.
    Step,
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
        // bounded: one pass over the bytecode during region plan construction
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
    /// Registers the facts are kept for, by fact index. A region's are
    /// fixed by a first scan before the solving scans
    /// (`NumericLoop::recognize`), so a fact exists before the
    /// instruction that first names its register.
    facts: Vec<u16>,
    /// The layout was seeded by an earlier scan: no new register, fact or
    /// record may appear.
    frozen: bool,
    /// The facts at the instruction being scanned.
    cur: Facts,
    /// The instruction being scanned is reached only by a jump: the one
    /// before it does not fall through.
    dead: bool,
    /// Forward edges not yet reached: target pc and the facts on the edge.
    pending: Vec<(usize, Facts)>,
    /// Loop heads (the pc after a `FOR_PREP`) with the facts their back
    /// edges were found to carry, kept across scans.
    heads: Vec<(usize, Facts)>,
    /// A back edge carried facts its head did not assume: scan again.
    restart: bool,
    /// The facts the region's own head assumed.
    outer_entry: Facts,
    /// Loops open at the instruction being scanned, innermost last.
    loops: Vec<OpenLoop>,
    /// Nested loops, by `Prep` order.
    inner: Vec<InnerLoop>,
    /// Registers any operation writes on any path: a number write, a record
    /// read, a method lookup's two destinations, a loop's controls.
    wrote: Vec<u16>,
    /// The lowest register any call clears.
    min_cleared: Option<u16>,
    /// Registers some record read of the body defines, by record id.
    records: Vec<u16>,
    record_sites: Vec<RecordSite>,
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
    /// operation may touch that register in between, and no edge may
    /// leave, enter or cross the interval.
    loading: Option<(u16, Callee)>,
    /// The loading operation's index and pc.
    loading_at: (usize, usize),
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

    /// A layout seeded with the registers, facts and records of an earlier
    /// scan of the same body, and the loop-head facts found so far.
    fn seeded(seed: Option<(Vec<u16>, Vec<u16>, Vec<u16>)>, heads: Vec<(usize, Facts)>) -> Self {
        let mut layout = Layout {
            heads,
            ..Layout::default()
        };
        if let Some((registers, facts, records)) = seed {
            layout.registers = registers;
            layout.facts = facts;
            layout.records = records;
            layout.frozen = true;
        }
        layout
    }

    fn register(&mut self, register: u16) -> Option<u8> {
        if let Some(slot) = self.registers.iter().position(|&r| r == register) {
            return Some(mirror_slot(slot));
        }
        if self.frozen || self.full() {
            return None;
        }
        self.registers.push(register);
        Some(mirror_slot(self.registers.len() - 1))
    }

    /// `register`'s fact index.
    fn fact(&mut self, register: u16) -> Option<usize> {
        if let Some(fact) = self.facts.iter().position(|&r| r == register) {
            return Some(fact);
        }
        if self.frozen || self.facts.len() >= 64 {
            return None;
        }
        self.facts.push(register);
        Some(self.facts.len() - 1)
    }

    /// `register` now holds something other than a record or a number: its
    /// record facts die, and it counts as written for the receiver check.
    fn overwritten(&mut self, register: u16) -> Option<()> {
        let bit = 1u64 << self.fact(register)?;
        self.cur.rmust &= !bit;
        self.cur.rmay &= !bit;
        self.note_write(register);
        Some(())
    }

    fn note_write(&mut self, register: u16) {
        if !self.wrote.contains(&register) {
            self.wrote.push(register);
        }
    }

    /// The stored facts of loop head `head`, met into `facts`.
    fn at_head(&self, head: usize, facts: Facts) -> Facts {
        self.heads
            .iter()
            .find(|&&(pc, _)| pc == head)
            .map_or(facts, |&(_, stored)| facts.meet(stored))
    }

    /// A back edge to loop head `head`, which assumed `entry`, carrying
    /// `taken`: when that narrows the head's facts, store them and mark the
    /// region for another scan.
    fn back_edge(&mut self, head: usize, entry: Facts, taken: Facts) {
        let met = entry.meet(taken);
        if met == entry {
            return;
        }
        self.restart = true;
        match self.heads.iter_mut().find(|(pc, _)| *pc == head) {
            Some((_, stored)) => *stored = stored.meet(met),
            None => self.heads.push((head, met)),
        }
    }

    /// The facts after number writes of `registers`, from `facts`.
    fn written_facts(&mut self, mut facts: Facts, registers: &[u16]) -> Option<Facts> {
        // bounded: at most two loop counters
        for &register in registers {
            facts = facts.wrote(self.fact(register)?);
        }
        Some(facts)
    }

    /// A register operand being read: guarded unless written earlier. A
    /// provisional upvalue load read as a number becomes a number read
    /// once at entry.
    fn read(&mut self, register: u16) -> Option<u8> {
        if let Some((loaded, Callee::Upvalue(index))) = self.loading
            && loaded == register
        {
            self.hoist_upvalue(register, index)?;
        }
        if self.untouchable(register) || self.tables.iter().any(|&(r, _)| r == register) {
            return None;
        }
        let bit = 1u64 << self.fact(register)?;
        if self.cur.safe & bit == 0 || self.cur.rmay & bit != 0 {
            return None;
        }
        let slot = self.register(register)?;
        if self.cur.must & bit == 0 {
            self.guards |= 1 << slot;
        }
        Some(slot)
    }

    /// Turn the open `GET_UPVAL register u` load into a copy of upvalue
    /// `u`, read once at entry, at the load's own operation. No edge
    /// entered or crossed the interval (`join` and `branch` refuse while a
    /// load is open) and nothing touched the register in it, so the write
    /// counts from the load.
    fn hoist_upvalue(&mut self, register: u16, index: u16) -> Option<()> {
        let b = self.constant(Constant::Upvalue(index), None)?;
        self.loading = None;
        let a = self.write(register)?;
        let (op, _) = self.loading_at;
        *self.ops.get_mut(op)? = LoopOp {
            kind: OpKind::Copy,
            a,
            b,
            c: 0,
        };
        Some(())
    }

    fn write(&mut self, register: u16) -> Option<u8> {
        if self.untouchable(register) {
            return None;
        }
        let slot = self.register(register)?;
        let fact = self.fact(register)?;
        self.written |= 1 << slot;
        self.cur = self.cur.wrote(fact);
        self.tables.retain(|&(r, _)| r != register);
        self.note_write(register);
        Some(slot)
    }

    fn record_id(&self, register: u16) -> Option<usize> {
        self.records.iter().position(|&r| r == register)
    }

    /// The record id `register` holds on every path here, as a field
    /// receiver; `None` when some path reaching here leaves it no record of
    /// this pass.
    fn record_receiver(&mut self, register: u16) -> Option<u8> {
        // A loaded callee, or a method's receiver copy, is no record.
        if self.untouchable(register) {
            return None;
        }
        let id = self.record_id(register)?;
        let bit = 1u64 << self.fact(register)?;
        (self.cur.rmust & bit != 0).then_some(id as u8)
    }

    /// The record site of record `record` and `key`, shared with earlier
    /// constituents naming both.
    fn record_site(&mut self, record: u8, key: u16, store: bool) -> Option<u8> {
        let index = match self
            .record_sites
            .iter()
            .position(|site| site.record == record && site.key == key)
        {
            Some(index) => index,
            None => {
                if self.record_sites.len() >= MAX_RECORD_SITES {
                    return None;
                }
                self.record_sites.push(RecordSite {
                    record,
                    key,
                    store: false,
                });
                self.record_sites.len() - 1
            }
        };
        self.record_sites[index].store |= store;
        Some(index as u8)
    }

    /// Whether the instruction at `next` uses register `register` as a field
    /// receiver, which makes a `GET_TABLE` into it a record read. The
    /// dataflow checks the use again, so this only selects the form.
    fn receiver_next(code: &[Instr], mut next: usize, end: Option<usize>, register: u16) -> bool {
        // A literal stored into the record is loaded first.
        // bounded: the scan stops at the body's end
        while end.is_some_and(|end| next < end)
            && code
                .get(next)
                .is_some_and(|load| load.opcode() == Instr::OP_LOAD_NUM && load.a() != register)
        {
            next += 1;
        }
        end.is_some_and(|end| next < end)
            && code.get(next).is_some_and(|use_| {
                ((use_.opcode() == Instr::OP_GET_FIELD || use_.opcode() == Instr::OP_SELF)
                    && use_.b() == register)
                    || (use_.opcode() == Instr::OP_SET_FIELD && use_.a() == register)
            })
    }

    /// A record read of `receiver` by `GET_TABLE` instruction `inst`, whose
    /// exit resumes canonical dispatch at `exit`. The key is read before the
    /// destination becomes a record, as dispatch reads it. A loop control
    /// register never holds a record.
    fn record(&mut self, receiver: Receiver, exit: usize, inst: Instr) -> Option<LoopOp> {
        if self.indexed.len() >= MAX_INDEXED {
            return None;
        }
        let c = self.read(inst.c())?;
        let register = inst.a();
        if self.registers.iter().take(4).any(|&r| r == register) || self.untouchable(register) {
            return None;
        }
        let id = match self.record_id(register) {
            Some(id) => id,
            None => {
                if self.frozen || self.records.len() >= MAX_RECORDS {
                    return None;
                }
                self.records.push(register);
                self.records.len() - 1
            }
        };
        let fact = self.fact(register)?;
        let bit = 1u64 << fact;
        self.cur = self.cur.wrote(fact);
        self.cur.rmust |= bit;
        self.cur.rmay |= bit;
        self.note_write(register);
        self.indexed.push((receiver, exit));
        Some(LoopOp {
            kind: OpKind::Record,
            a: id as u8,
            b: (self.indexed.len() - 1) as u8,
            c,
        })
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
    /// constituents naming both, used as `use_` says.
    fn site(
        &mut self,
        source: TableSource,
        key: u16,
        cache: Option<u8>,
        use_: SiteUse,
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
        match use_ {
            SiteUse::Store => {
                site.store = true;
                site.set_cache = site.set_cache.or(cache);
            }
            SiteUse::Number | SiteUse::Truth => {
                site.number |= use_ == SiteUse::Number;
                site.read_cache = site.read_cache.or(cache);
            }
        }
        Some(index as u8)
    }

    /// `GET_FIELD d s k` at `pc` fused with the `TEST_FALSE d` or
    /// `TEST_TRUE d` after it, in a leaf ending at `end`: a branch on the
    /// truthiness of field site `(s, k)`'s live value. The receiver is
    /// classified before `d` is clobbered (`d` may be `s`). `d` then holds
    /// a value the mirror does not represent: its `must` and `safe` facts
    /// are cleared, as a call clearing it would, so a number read of it on
    /// any path still reaching it from here refuses (`safe` meets as an
    /// intersection at joins) until a number write restores them; it is
    /// reserved in the layout so `Layout::receiver` never takes it for an
    /// entry parameter, and loses any table binding. The edge is decoded
    /// from the `TEST`, and no edge may enter the `TEST` half: an edge
    /// there would test `d` without the lookup, and stays pending, which
    /// refuses the plan.
    fn truth_test(
        &mut self,
        inst: Instr,
        test: Instr,
        pc: usize,
        end: Option<usize>,
    ) -> Option<LoopOp> {
        let source = self.receiver(inst.b())?;
        let b = self.site(source, inst.c(), inst.cache_slot(), SiteUse::Truth)?;
        let register = inst.a();
        self.register(register)?;
        let bit = 1u64 << self.fact(register)?;
        self.cur.must &= !bit;
        self.cur.safe &= !bit;
        self.tables.retain(|&(r, _)| r != register);
        self.note_write(register);
        self.branch(pc + 1, test.sj(), end)?;
        Some(LoopOp {
            kind: OpKind::Test(test.opcode() == Instr::OP_TEST_TRUE),
            a: 0,
            b,
            c: 0,
        })
    }

    /// `GET_GLOBAL r k`, `GET_UPVAL r u` or a method lookup loading a
    /// callee into `r` (and a method's receiver into `r + 1`). Off under
    /// fault injection, whose call-depth and stack-space sites count every
    /// admission.
    fn load(&mut self, register: u16, callee: Callee, pc: usize) -> Option<LoopOp> {
        // An upvalue load still open when another load starts is no callee
        // of a supported call: the call of that register would come after
        // the second load's own call, or with the second load as its
        // argument register, and `call_site` refuses both. So it is a number
        // read once at entry, as `Layout::read` would make it at its first
        // read; were it a callee after all, the `CALL` of its register finds
        // no open load and refuses the region. This takes the operands of
        // `up + (up - x)`, which codegen loads back to back.
        if let Some((loaded, Callee::Upvalue(index))) = self.loading {
            self.hoist_upvalue(loaded, index)?;
        }
        if cfg!(feature = "fault-injection")
            || self.loading.is_some()
            || self.calls.len() >= MAX_CALLS
            || self.untouchable(register)
        {
            return None;
        }
        // The destinations now hold a callee (and a method's receiver), no
        // record of this pass. Their `must` and `safe` facts stand only
        // because nothing may read them before the `CALL` or the hoist that
        // ends the interval writes or clears them: a second load, any edge
        // into or out of the interval, a loop step and an open load at the
        // end are all refused.
        if callee.method() {
            let copy = register.checked_add(1)?;
            self.method_writes.extend([register, copy]);
            self.overwritten(copy)?;
        }
        self.overwritten(register)?;
        self.register(register)?;
        self.loading = Some((register, callee));
        self.loading_at = (self.ops.len(), pc);
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
        // bounded: the fixed argument count encoded in the call instruction
        for offset in 1..=u16::from(count) {
            args.push(if method && offset == 1 {
                0
            } else {
                self.read(register.checked_add(offset)?)?
            });
        }
        self.loading = None;
        // Everything above `r`, and `r` too without a result, is nil after
        // the call: no longer safe to read, and no record.
        let cleared = register.checked_add(u16::from(results))?;
        self.min_cleared = Some(self.min_cleared.map_or(cleared, |m| m.min(cleared)));
        let killed = self
            .facts
            .iter()
            .enumerate()
            .filter(|&(_, &r)| r >= cleared)
            .fold(0u64, |mask, (fact, _)| mask | 1 << fact);
        self.cur.safe &= !killed;
        self.cur.rmust &= !killed;
        self.cur.rmay &= !killed;
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
            load_pc: self.loading_at.1,
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
        let mut joined = false;
        let mut facts = if self.dead { Facts::TOP } else { self.cur };
        self.pending.retain(|&(target, at_edge)| {
            if target == pc {
                joined = true;
                facts = facts.meet(at_edge);
                false
            } else {
                true
            }
        });
        // No edge may enter a callee load's interval.
        if !(joined || !self.dead) || (joined && self.loading.is_some()) {
            return None;
        }
        self.cur = facts;
        self.dead = false;
        Some(())
    }

    /// A forward branch or jump at `pc` with offset `offset`, inside a body
    /// ending at `end`; its target joins the facts as they stand now. No
    /// edge may leave a callee load's interval.
    fn branch(&mut self, pc: usize, offset: i32, end: Option<usize>) -> Option<()> {
        let end = end?;
        let target = Frame::jumped(pc + 1, offset);
        if target <= pc || target > end || self.loading.is_some() {
            return None;
        }
        self.pending.push((target, self.cur));
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
        // bounded: pc advances until the region end or the end of the bytecode
        while end.is_none_or(|end| pc < end) {
            let inst = *code.get(pc)?;
            let opcode = inst.opcode();
            self.join(pc)?;
            self.op_at.push((pc, self.ops.len()));
            if self.loops.last().is_some_and(|open| open.step_pc == pc) {
                self.step(cost, pc)?;
                pc += 1;
                continue;
            }
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
                        // An exit resumes at `GET_UPVAL`, which installs the
                        // table the region read without writing `t`.
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_GET_TABLE).is_none());
                        }
                        let receiver = Receiver::Upvalue(inst.b());
                        let op = if Self::receiver_next(code, pc + 2, end, index.a()) {
                            self.record(receiver, pc, index)?
                        } else {
                            self.index(receiver, pc, index)?
                        };
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
                            pc,
                        )?;
                        self.ops.push(op);
                        self.charges.push((pc + 2, 0));
                        pc += 2;
                        continue;
                    }
                    // `SELF r t k` on a record: the method is looked up on
                    // every pass.
                    Instr::OP_SELF if fields && self.record_id(inst.b()).is_some() => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_SELF).is_none());
                        }
                        let record = self.record_receiver(inst.b())?;
                        self.load(
                            inst.a(),
                            Callee::Dynamic {
                                record,
                                key: inst.c(),
                            },
                            pc,
                        )?
                    }
                    // A nested numeric loop.
                    Instr::OP_FOR_PREP if fields && end.is_some() => self.prep(code, pc, end)?,
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
                            pc,
                        )?
                    }
                    Instr::OP_GET_TABLE if fields => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_TABLE).is_none());
                        }
                        self.receivers.push(inst.b());
                        let receiver = Receiver::Register(inst.b());
                        if Self::receiver_next(code, pc + 1, end, inst.a()) {
                            self.record(receiver, pc, inst)?
                        } else {
                            self.index(receiver, pc, inst)?
                        }
                    }
                    // A field of a record, read per iteration; the receiver
                    // is classified before the destination is written.
                    Instr::OP_GET_FIELD if fields && self.record_id(inst.b()).is_some() => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_FIELD).is_none());
                        }
                        let record = self.record_receiver(inst.b())?;
                        let b = self.record_site(record, inst.c(), false)?;
                        LoopOp {
                            kind: OpKind::RecordRead,
                            a: self.write(inst.a())?,
                            b,
                            c: 0,
                        }
                    }
                    // A store of a number into a field of a record.
                    Instr::OP_SET_FIELD if fields => {
                        let record = self.record_receiver(inst.a())?;
                        let c = self.read(inst.c())?;
                        let b = self.record_site(record, inst.b(), true)?;
                        LoopOp {
                            kind: OpKind::RecordStore,
                            a: 0,
                            b,
                            c,
                        }
                    }
                    // Forward compare-branches and jumps, in a loop body or
                    // a leaf alike.
                    Instr::OP_BR_LT..=Instr::OP_BR_NE => {
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
                    Instr::OP_BR_LT_RK..=Instr::OP_BR_NE_RK => {
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
                    Instr::OP_JMP => {
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
                        pc,
                    )?,
                    // A callee load, or, read next as a number, a number
                    // upvalue read once at entry (`Layout::read`).
                    Instr::OP_GET_UPVAL if fields => {
                        self.load(inst.a(), Callee::Upvalue(inst.b()), pc)?
                    }
                    Instr::OP_CALL if fields => self.call_site(inst, pc)?,
                    // A leaf's upvalue load defines a table receiver and
                    // writes no mirror slot; it is free, so it has no charge.
                    // The table bindings are not kept per path, so a load
                    // after the first branch, which may not dominate a use
                    // past a join, refuses.
                    Instr::OP_GET_UPVAL if leaf && self.jumps.is_empty() => {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_UPVAL).is_none());
                        }
                        let register = inst.a();
                        self.tables.retain(|&(r, _)| r != register);
                        self.tables.push((register, TableSource::Upvalue(inst.b())));
                        pc += 1;
                        continue;
                    }
                    // `GET_FIELD d s k; TEST_FALSE d` or `TEST_TRUE d`, both
                    // inside the body: a truth test of the field. Both are
                    // free, so no charge falls between them.
                    Instr::OP_GET_FIELD
                        if leaf
                            && end.is_some_and(|end| pc + 1 < end)
                            && code.get(pc + 1).is_some_and(|test| {
                                matches!(test.opcode(), Instr::OP_TEST_FALSE | Instr::OP_TEST_TRUE)
                                    && test.a() == inst.a()
                            }) =>
                    {
                        const {
                            assert!(Instr::fixed_charge(Instr::OP_GET_FIELD).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_TEST_FALSE).is_none());
                            assert!(Instr::fixed_charge(Instr::OP_TEST_TRUE).is_none());
                        }
                        let op = self.truth_test(inst, code[pc + 1], pc, end)?;
                        self.ops.push(op);
                        self.charges.push((pc + 2, 0));
                        pc += 2;
                        continue;
                    }
                    Instr::OP_GET_FIELD if leaf => {
                        let source = self.receiver(inst.b())?;
                        let b = self.site(source, inst.c(), inst.cache_slot(), SiteUse::Number)?;
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
                        let b = self.site(source, inst.b(), inst.cache_slot(), SiteUse::Store)?;
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
        if !self.pending.is_empty() || !self.loops.is_empty() {
            return None;
        }
        Some(pc)
    }

    /// A nested loop's `FOR_PREP` at `pc` inside a body ending at `end`.
    /// It reads its three controls; when it does not enter it goes past
    /// its `FOR_LOOP` with nothing written, as dispatch does, and when it
    /// does it writes all four. No edge from outside may enter the loop's
    /// body or reach its `FOR_LOOP`.
    fn prep(&mut self, code: &[Instr], pc: usize, end: Option<usize>) -> Option<LoopOp> {
        let end = end?;
        let inst = code[pc];
        let after = pc + 1;
        let exit = Frame::jumped(after, inst.sj());
        if exit <= after || exit > end {
            return None;
        }
        let step_pc = exit - 1;
        let step = *code.get(step_pc)?;
        if step.opcode() != Instr::OP_FOR_LOOP
            || step.a() != inst.a()
            || Frame::jumped(exit, step.sj()) != after
            || self.loops.len() >= MAX_DEPTH
            || self
                .loops
                .last()
                .is_some_and(|open| step_pc >= open.step_pc)
            || self.inner.len() >= MAX_INNER
            || self
                .pending
                .iter()
                .any(|&(target, _)| target > pc && target <= step_pc)
        {
            return None;
        }
        let base = inst.a();
        let registers = [
            base,
            base.checked_add(1)?,
            base.checked_add(2)?,
            base.checked_add(3)?,
        ];
        self.read(registers[2])?;
        self.read(registers[1])?;
        self.read(registers[0])?;
        if self.loading.is_some() {
            return None;
        }
        // The edge past the loop when it does not enter.
        self.pending.push((exit, self.cur));
        let mut controls = [0u8; 4];
        // bounded: the four numeric loop controls
        for (slot, &register) in controls.iter_mut().zip(&registers) {
            *slot = self.write(register)?;
        }
        self.cur = self.at_head(after, self.cur);
        let index = self.inner.len();
        self.inner.push(InnerLoop {
            controls,
            step_pc,
            body: self.ops.len() + 1,
            exit: 0,
        });
        self.loops.push(OpenLoop {
            index,
            base,
            step_pc,
            head: after,
            entry: self.cur,
        });
        Some(LoopOp {
            kind: OpKind::Prep,
            a: 0,
            b: index as u8,
            c: 0,
        })
    }

    /// The innermost open loop's `FOR_LOOP` at `pc`: it reads the three
    /// controls; its taken edge writes the counter and the visible
    /// variable and returns to the head, its fall-through writes nothing.
    fn step(&mut self, cost: &CostState, pc: usize) -> Option<()> {
        let open = self.loops.pop()?;
        let base = open.base;
        self.read(base.checked_add(2)?)?;
        self.read(base.checked_add(1)?)?;
        self.read(base)?;
        if self.loading.is_some() {
            return None;
        }
        let taken = self.written_facts(self.cur, &[base, base.checked_add(3)?])?;
        self.back_edge(open.head, open.entry, taken);
        self.ops.push(LoopOp {
            kind: OpKind::Step,
            a: 0,
            b: open.index as u8,
            c: 0,
        });
        self.charges
            .push(Self::charge(cost, Instr::OP_FOR_LOOP, pc));
        Some(())
    }

    /// The finished body. `None` if a field receiver register is written by
    /// the body or is one of the first `reserved` registers (loop controls).
    fn finish(mut self, reserved: usize, tail: (usize, u64)) -> Option<(Body, Vec<u16>)> {
        if self.loading.is_some() {
            return None;
        }
        let control = !self.jumps.is_empty()
            || !self.indexed.is_empty()
            || !self.inner.is_empty()
            || self.calls.iter().any(|site| site.callee.dynamic());
        // Invariant receivers are read once, so no record may replace one.
        // What a pass leaves for the next (a record, a cleared register) is
        // the loop heads' business: their facts are met with the back
        // edges' (`Layout::back_edge`).
        // bounded: at most MAX_RECORDS record registers
        for &register in &self.records {
            if self.receivers.contains(&register) {
                return None;
            }
        }
        let records = self
            .records
            .iter()
            .map(|&register| {
                let slot = self.registers.iter().position(|&r| r == register);
                (register, slot.map(mirror_slot))
            })
            .collect::<Vec<_>>();
        // Operation indices fit a `u8`.
        if control && self.ops.len() > usize::from(u8::MAX) {
            return None;
        }
        // bounded: at most one forward jump per bytecode instruction
        for &(op, target) in &self.jumps {
            let (_, index) = self.op_at.iter().find(|&&(pc, _)| pc == target)?;
            self.ops[op].a = *index as u8;
        }
        // bounded: at most MAX_INNER nested loops
        for inner in &mut self.inner {
            let exit = inner.step_pc + 1;
            let (_, index) = self.op_at.iter().find(|&&(pc, _)| pc == exit)?;
            inner.exit = *index;
        }
        // An entry-resolved receiver keeps its register's value for the whole
        // region: nothing writes it (a number, a record, a method lookup's
        // destinations, a loop's controls) and no call clears it.
        // bounded: the receivers recorded during the bytecode scan
        for &receiver in &self.receivers {
            if let Some(slot) = self.registers.iter().position(|&r| r == receiver)
                && (slot < reserved || self.written & (1 << slot) != 0)
            {
                return None;
            }
            if self.method_writes.contains(&receiver)
                || self.wrote.contains(&receiver)
                || self.min_cleared.is_some_and(|cleared| receiver >= cleared)
            {
                return None;
            }
        }
        // bounded: at most MAX_CALLS call sites
        for site in &mut self.calls {
            let cleared = site.cleared();
            site.above = self
                .registers
                .iter()
                .enumerate()
                .filter(|&(_, &r)| r >= cleared)
                .fold(0, |mask, (slot, _)| mask | 1 << slot);
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
        // bounded: the operations emitted by one bytecode scan
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
                records: records.into_boxed_slice(),
                record_sites: self.record_sites.into_boxed_slice(),
                inner: self.inner.into_boxed_slice(),
                control,
            },
            registers,
        ))
    }
}

/// The operations one run of a leaf body made, one bit per operation
/// index: every bit for a straight-line body. A leaf with branches holds at
/// most `MAX_CONTROL_LEAF_OPS` operations, so each has a bit.
type LeafPath = u64;

/// Every operation of a straight-line leaf.
const FULL_PATH: LeafPath = u64::MAX;

impl Body {
    /// The charges a leaf run along `path` makes, in order: its operations'
    /// then `RETURN`'s. A leaf only branches forward, so path order is
    /// operation order.
    fn path_charges(&self, path: LeafPath) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.charges
            .iter()
            .enumerate()
            .filter(move |&(index, _)| index >= 64 || path & (1 << index) != 0)
            .map(|(_, &charge)| charge)
            .chain(std::iter::once(self.tail))
    }

    /// The sum of a leaf run's charges along `path`. `weight`, the sum of
    /// every charge, bounds it.
    fn path_weight(&self, path: LeafPath) -> u64 {
        if path == FULL_PATH {
            return self.weight;
        }
        self.path_charges(path)
            .fold(0, |sum, (_, weight)| sum.saturating_add(weight))
    }

    /// Record every positive charge of one pass (of a leaf, along `path`)
    /// in the charge-site trace, as per-instruction dispatch would.
    #[cfg(feature = "cost-check-interval")]
    fn record_charges(&self, path: LeafPath) {
        // bounded: one body's charge sites plus its tail charge
        for (cursor, weight) in self.path_charges(path) {
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
        let controls = [
            base,
            base.checked_add(1)?,
            base.checked_add(2)?,
            base.checked_add(3)?,
        ];
        // The first scan fixes the registers, facts and records; each later
        // scan solves the loop heads' facts from the back edges found so
        // far, until a scan finds every head's facts unchanged.
        let mut seed = None;
        let mut heads = Vec::new();
        // bounded: at most MAX_SCANS scans of one region
        for scan in 0..MAX_SCANS {
            let mut layout = Layout::seeded(seed.take(), std::mem::take(&mut heads));
            // bounded: four numeric loop control registers
            for &register in &controls {
                layout.register(register)?;
                layout.fact(register)?;
            }
            layout.cur = layout.at_head(after, Facts::ENTRY);
            layout.outer_entry = layout.cur;
            layout.scan(bytecode, cost, after, Some(step_pc), Mode::Region)?;
            // The loop step reads the three hidden controls after the body,
            // guarded unless every path to it writes them; its taken edge
            // writes the counter and the visible variable.
            layout.read(controls[2])?;
            layout.read(controls[1])?;
            layout.read(controls[0])?;
            let taken = layout.written_facts(layout.cur, &[controls[0], controls[3]])?;
            let entry = layout.outer_entry;
            layout.back_edge(after, entry, taken);
            if scan == 0 || layout.restart {
                heads = std::mem::take(&mut layout.heads);
                seed = Some((
                    std::mem::take(&mut layout.registers),
                    std::mem::take(&mut layout.facts),
                    std::mem::take(&mut layout.records),
                ));
                continue;
            }
            let tail = Layout::charge(cost, Instr::OP_FOR_LOOP, step_pc);
            let (body, _) = layout.finish(4, tail)?;
            return Some(Self {
                prep,
                step_pc,
                body,
                backoff: AtomicU8::new(0),
            });
        }
        None
    }
}

/// A function whose code from pc 0 to its first `RETURN` is a numeric body,
/// possibly reading, truth-testing and storing fields of receiver tables
/// and branching forward to at most that `RETURN`, returning one
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
        // The body runs to the first `RETURN`, which every forward edge of
        // the body must reach: one past it would run code the plan does not
        // model.
        let end = bytecode
            .code
            .iter()
            .position(|inst| inst.opcode() == Instr::OP_RETURN)?;
        let mut layout = Layout::default();
        let ret_pc = layout.scan(bytecode, cost, 0, Some(end), Mode::Leaf)?;
        let ret = *bytecode.code.get(ret_pc)?;
        if ret_pc != end || (!layout.jumps.is_empty() && layout.ops.len() > MAX_CONTROL_LEAF_OPS) {
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
        // bounded: at most MIRROR_SLOTS registers
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

/// One class's entry in a region-local method cache of a method call on a
/// record (`Callee::Dynamic`): the receiver metatable it was found through,
/// the method, its runtime (which holds the leaf plan) and its field sites.
struct RegionMethod {
    meta: ObjectPtr,
    method: Val,
    runtime: Arc<BytecodeRuntime>,
    /// The leaf's upvalue field sites, resolved at admission.
    fields: FieldSlots,
    /// Each field site's key.
    keys: [Val; MAX_FIELD_SITES],
    /// Each receiver site's last slot, a position hint as a record site's.
    hints: FieldSlots,
    has_fields: bool,
}

impl RegionMethod {
    fn leaf(&self) -> &NumericLeaf {
        self.runtime
            .numeric_leaf
            .as_ref()
            .expect("admit_method found a leaf plan")
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
            let body = &region.body;
            match (
                body.records.is_empty(),
                body.calls.is_empty(),
                body.inner.is_empty(),
            ) {
                (true, true, true) => {
                    self.run_control_region::<false, false, false>(region, frame, ip, local_cost)
                }
                (true, true, false) => {
                    self.run_control_region::<false, false, true>(region, frame, ip, local_cost)
                }
                (false, true, true) => {
                    self.run_control_region::<true, false, false>(region, frame, ip, local_cost)
                }
                (false, true, false) => {
                    self.run_control_region::<true, false, true>(region, frame, ip, local_cost)
                }
                (true, false, true) => {
                    self.run_control_region::<false, true, false>(region, frame, ip, local_cost)
                }
                (true, false, false) => {
                    self.run_control_region::<false, true, true>(region, frame, ip, local_cost)
                }
                (false, false, true) => {
                    self.run_control_region::<true, true, false>(region, frame, ip, local_cost)
                }
                (false, false, false) => {
                    self.run_control_region::<true, true, true>(region, frame, ip, local_cost)
                }
            }
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
        // The straight-line runner charges each leaf's whole weight; a
        // leaf with branches charges its path, which only the control
        // runner and `try_numeric_leaf` follow. Declining runs the loop
        // canonically, each call still taking the leaf.
        if CALLS
            && callees
                .iter()
                .flatten()
                .any(|resolved| resolved.leaf().body.control)
        {
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
            // Every leaf here is straight-line (`run_region`), so its whole
            // weight is what it charges.
            let fast = weight == 0 || weight < self.cost_flush_at().saturating_sub(*local_cost);
            if !CALLS && fast {
                if weight != 0 {
                    *local_cost += weight;
                    #[cfg(feature = "cost-check-interval")]
                    body.record_charges(FULL_PATH);
                }
                // bounded: one pass over the region body's operations
                for op in &body.ops {
                    x[at(op.a)] = eval(op, &x);
                    written |= 1 << op.a;
                }
            } else if !CALLS {
                // bounded: one pass over the region body's operations and charges
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
                // bounded: one pass over the region body's operations and charges
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
                            let (result, _) = match fields {
                                None => callee.eval::<false>(argument),
                                Some(fields) => self.run_leaf_fields(callee, fields, argument),
                            };
                            if !fast {
                                // bounded: one leaf body's charge sites plus its tail charge
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

    /// `run_numeric_loop` for a body with forward branches, indexed reads,
    /// records, nested loops or method calls on a record. The operations
    /// run by index, a branch or jump moving the index forward to its
    /// target, a nested loop's taken back edge moving it back.
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
    ///
    /// Records (`RECORDS`, compiled apart so other bodies keep passes free
    /// of their arms): a `GET_TABLE` whose next use of its destination is
    /// a field receiver reads a record, a raw element that must be a table,
    /// else the region exits at the read's first instruction as above. The
    /// recognizer proves every field access of a record is reached only
    /// through a record read of the same pass on every path, that a record
    /// is never read as a number or stored, that no register read before
    /// written in a pass may still hold one, and that no record register is
    /// a loop control or an invariant receiver. The record goes onto the
    /// stack at once, as canonical dispatch leaves it, and clears its
    /// register's mirror bits, so no exit publishes an older number over it
    /// and the next checkpoint republishes a number written after it.
    /// - A record field read takes the key's live slot raw (no `__index`
    ///   consulted, as for any raw hit) and must hold a number; otherwise
    ///   the region exits at the free `GET_FIELD`.
    /// - A record store resolves the key's live slot before its
    ///   `TableWrite` charge; a key not live there exits at the `SET_FIELD`
    ///   with nothing charged for it, and canonical dispatch charges and
    ///   stores with full semantics (`__newindex`, a new key). Otherwise the
    ///   charge, which may refuse with no store made, then
    ///   `GcHeap::overwrite_live_slot`, the overwrite a cached `SET_FIELD`
    ///   makes. Stores write numbers into live slots under string keys, so
    ///   no key is created or removed and no table version moves: number
    ///   keyed element reads of the tables resolved at entry are unchanged,
    ///   entry-read fields are guarded by the key check at entry
    ///   (`State::resolve_record_keys`, which also declines `__index`), and
    ///   record reads re-read the live slot, so aliased records (two records
    ///   one table, a record its own source) are exact.
    /// - A site's last slot is reused while the table and version are the
    ///   same, else as a key-checked position (`Table::renew_slot`), which
    ///   records of one layout share, else looked up. Inline caches are
    ///   neither consulted nor refilled; they only accelerate.
    ///
    /// Nested loops: an inner `FOR_PREP` (`Prep`) reads its controls from
    /// the mirror, numbers by the entry guards, and either writes all four
    /// or goes past its `FOR_LOOP` with nothing written; an inner
    /// `FOR_LOOP` (`Step`) computes the next counter, and on the taken edge
    /// publishes the slots first written since the last checkpoint, polls
    /// with the cursor past it, and only then writes the counters, as the
    /// region's own step does. Every back edge ends a charge segment: the
    /// fast test is made again there, and between two of them each
    /// operation runs at most once, so the sum of every weight still bounds
    /// a segment. The recognizer solves the facts at each loop head to a
    /// fixpoint over its back edges, so a read the second iteration makes
    /// is proven as the first's.
    ///
    /// Calls (`CALLS`): as in the straight-line runner, a call runs its
    /// leaf on the mirror after reading the poll flag and, in a
    /// per-operation segment, after `CostState::admits_charges` admits the
    /// whole leaf, else exits at the `CALL` with the callee and receiver
    /// put back last. The recognizer tracks per path which registers a
    /// call left nil, so a branch beside a call is exact. A method call on
    /// a record (`Callee::Dynamic`) looks its method up on every pass: an
    /// own raw value of the key exits at the free `SELF`; otherwise the
    /// receiver's metatable selects an entry of the site's region-local
    /// cache, filled by `State::region_method` and `State::admit_method`
    /// on a miss. An entry stays exact for the whole run: no store of the
    /// region or of any admitted leaf is under `__index` or under a key the
    /// region relies on (method keys, callee names, entry-read fields), and
    /// nothing creates or removes a key or sets a metatable, so every link
    /// of the route answers the same. The leaf's receiver fields are found
    /// per pass on the record, as record sites are, and every read one
    /// must hold a number before the leaf runs, else the region exits at
    /// the `CALL`. In a fast segment a method's leaf weight joins the bound
    /// (`weight` still bounds what remains), else the segment turns
    /// per-operation. Cached metatables and methods stay reachable through
    /// the receivers, which the entry-resolved tables hold by number keys
    /// no region store can replace, and nothing allocates.
    #[inline(never)]
    fn run_control_region<const RECORDS: bool, const CALLS: bool, const NESTED: bool>(
        &mut self,
        region: &NumericLoop,
        frame: &Frame,
        ip: &mut usize,
        local_cost: &mut u64,
    ) -> Result<()> {
        let base = frame.stack_bottom;
        let body = &region.body;
        if CALLS {
            let backoff = region.backoff.load(Ordering::Relaxed);
            if backoff != 0 {
                region.backoff.store(backoff - 1, Ordering::Relaxed);
                return Ok(());
            }
        }
        let mut x = [0.0f64; MIRROR_SLOTS];
        if !self.load_mirror(body, frame, base, &mut x) {
            return Ok(());
        }
        let mut tables = [None::<ObjectPtr>; MAX_INDEXED];
        // bounded: at most MAX_INDEXED indexed reads
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
        let mut keys = [Val::Nil; MAX_RECORD_SITES];
        if RECORDS && !self.resolve_record_keys(body, frame, &mut keys) {
            return Ok(());
        }
        // Each record's table this pass, and each record site's last slot:
        // a position hint, compared by handle and never dereferenced on
        // another table without its key check.
        let mut callees: [Option<Resolved>; MAX_CALLS] = Default::default();
        let mut methods: [Vec<RegionMethod>; MAX_CALLS] = Default::default();
        // Per call site: the method and receiver a method lookup on a record
        // found this pass, and the cache entry.
        let mut current = [(Val::Nil, None::<ObjectPtr>, 0usize); MAX_CALLS];
        let mut method_keys = [Val::Nil; MAX_CALLS];
        // A method's field sites this pass, resolved before its leaf runs.
        let mut method_slots: FieldSlots = [None; MAX_FIELD_SITES];
        let mut relied = Vec::new();
        let mut weight = body.weight;
        if CALLS {
            if !self.resolve_callees(body, frame, &mut callees) {
                return Ok(());
            }
            if body.calls.iter().any(|site| site.callee.dynamic()) {
                #[cfg(feature = "disable-inline-caches")]
                if super::caches::hits_disabled() {
                    return Ok(());
                }
                relied = self.relied_keys(body, frame);
            }
            // bounded: at most MAX_CALLS call sites
            for ((site, resolved), key) in body.calls.iter().zip(&callees).zip(&mut method_keys) {
                if let Callee::Dynamic { key: literal, .. } = site.callee {
                    *key = frame.literal(literal);
                }
                if let Some(resolved) = resolved {
                    weight = weight.saturating_add(resolved.leaf().body.weight);
                }
            }
        }
        let mut records = [None::<ObjectPtr>; MAX_RECORDS];
        let mut found = [None::<(ObjectPtr, SlotRef)>; MAX_RECORD_SITES];
        let step_pc = region.step_pc;
        let mut written: u32 = 0;
        let mut published: u32 = 0;
        // The call site whose callee is loaded and not yet called, which an
        // exit puts back in its register.
        let mut loaded: Option<usize> = None;
        // A segment, from a pass's start or a back edge to the next back edge
        // or the pass's end, tests once whether it runs fast: kept apart, the
        // test stays out of the operation loop. A method call whose leaf
        // does not fit turns the rest of its segment per-operation.
        let mut slow;
        let mut first_pass = true;
        let result = 'pass: loop {
            let mut k = 0;
            slow = false;
            // bounded: each segment ends at a back edge, which polls, or at
            // the pass's end
            'segment: loop {
                let fast = !slow
                    && (weight == 0 || weight < self.cost_flush_at().saturating_sub(*local_cost));
                // bounded: branches only advance the operation index within this pass
                while let Some(op) = body.ops.get(k) {
                    let (cursor, charge) = body.charges[k];
                    // A store resolves its slot before its charge: a key not
                    // live there exits with nothing charged for it.
                    let mut store = None;
                    if RECORDS && let OpKind::RecordStore = op.kind {
                        let site = usize::from(op.b) % MAX_RECORD_SITES;
                        let table = records
                            [usize::from(body.record_sites[site].record) % MAX_RECORDS]
                            .expect("a record is defined on every path to its use");
                        match self.record_slot(table, keys[site], &mut found[site]) {
                            Some((slot, _)) => store = Some((table, slot)),
                            None => {
                                *ip = cursor - 1;
                                break 'pass Ok(());
                            }
                        }
                    }
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
                                .map(|tbl| tbl.get_number(x[at(op.c)]));
                            let Some(Val::Num(n)) = value else {
                                *ip = exit;
                                break 'pass Ok(());
                            };
                            x[at(op.a)] = n;
                            written |= 1 << op.a;
                        }
                        // Only a body with records holds these three.
                        OpKind::Record if RECORDS => {
                            let (_, exit) = body.indexed[usize::from(op.b)];
                            let table = tables[usize::from(op.b) % MAX_INDEXED]
                                .expect("every indexed receiver was resolved at entry");
                            let value = self
                                .heap
                                .as_table_ref(table)
                                .map(|tbl| tbl.get_number(x[at(op.c)]));
                            let Some(Val::Obj(record)) = value else {
                                *ip = exit;
                                break 'pass Ok(());
                            };
                            if self.heap.as_table_ref(record).is_none() {
                                *ip = exit;
                                break 'pass Ok(());
                            }
                            let id = usize::from(op.a) % MAX_RECORDS;
                            records[id] = Some(record);
                            let (register, slot) = body.records[id];
                            // The stack holds the record as canonical dispatch
                            // leaves it: no exit publishes an older number over
                            // it, and the next checkpoint republishes a number
                            // written after it.
                            self.stack[base + usize::from(register)] = Val::Obj(record);
                            if let Some(slot) = slot {
                                written &= !(1 << slot);
                                published &= !(1 << slot);
                            }
                        }
                        OpKind::RecordRead if RECORDS => {
                            let site = usize::from(op.b) % MAX_RECORD_SITES;
                            let table = records
                                [usize::from(body.record_sites[site].record) % MAX_RECORDS]
                                .expect("a record is defined on every path to its use");
                            let Some((_, Val::Num(n))) =
                                self.record_slot(table, keys[site], &mut found[site])
                            else {
                                *ip = cursor - 1;
                                break 'pass Ok(());
                            };
                            x[at(op.a)] = n;
                            written |= 1 << op.a;
                        }
                        OpKind::RecordStore if RECORDS => {
                            let (table, slot) = store.expect("the store resolved its slot");
                            // A live overwrite under a key proven not `__index`
                            // at entry, so inherited routes stay standing.
                            let stored =
                                self.heap
                                    .overwrite_live_slot(table, slot, Val::Num(x[at(op.c)]));
                            debug_assert!(stored, "a slot resolved just before stays live");
                        }
                        OpKind::Prep if NESTED => {
                            let inner = &body.inner[usize::from(op.b)];
                            let [start, limit, step, visible] = inner.controls;
                            let first = x[at(start)];
                            if super::eval_control::check_numeric_for_condition(
                                first,
                                x[at(limit)],
                                x[at(step)],
                            ) {
                                x[at(visible)] = first;
                                written |= 1 << start | 1 << limit | 1 << step | 1 << visible;
                            } else {
                                k = inner.exit;
                            }
                        }
                        OpKind::Step if NESTED => {
                            let inner = &body.inner[usize::from(op.b)];
                            let [counter, limit, step, visible] = inner.controls;
                            let by = x[at(step)];
                            let next = x[at(counter)] + by;
                            if super::eval_control::check_numeric_for_condition(
                                next,
                                x[at(limit)],
                                by,
                            ) {
                                *ip = inner.step_pc + 1;
                                let fresh = written & !published;
                                if fresh != 0 {
                                    self.publish_mirror(body, base, &x, fresh);
                                    published |= fresh;
                                }
                                if let Err(error) = self.poll_gc_checkpoint() {
                                    break 'pass Err(error);
                                }
                                x[at(counter)] = next;
                                x[at(visible)] = next;
                                written |= 1 << counter | 1 << visible;
                                slow = false;
                                k = inner.body;
                                continue 'segment;
                            }
                        }
                        OpKind::Load if CALLS => {
                            // The register holds the callee, not a number, until
                            // the call writes it.
                            let index = usize::from(op.b);
                            let site = &body.calls[index];
                            if let Callee::Dynamic { record, .. } = site.callee {
                                let table = records[usize::from(record) % MAX_RECORDS]
                                    .expect("a record is defined on every path to its use");
                                let Some(entry) = self.region_method_entry(
                                    table,
                                    method_keys[index],
                                    &mut methods[index],
                                    site,
                                    base,
                                    &relied,
                                ) else {
                                    if first_pass {
                                        region.backoff.store(BACKOFF_ENTRIES, Ordering::Relaxed);
                                    }
                                    *ip = site.load_pc;
                                    break 'pass Ok(());
                                };
                                current[index] = (methods[index][entry].method, Some(table), entry);
                            }
                            written &= !(1 << site.slot);
                            loaded = Some(index);
                        }
                        OpKind::Call if CALLS => {
                            let index = usize::from(op.b);
                            let site = &body.calls[index];
                            let (leaf, fields) = match &callees[index] {
                                Some(resolved) => (resolved.leaf(), resolved.fields.as_deref()),
                                None => {
                                    let (_, receiver, entry) = current[index];
                                    let receiver =
                                        receiver.expect("a method lookup preceded its call");
                                    if !self.method_fields(
                                        &mut methods[index][entry],
                                        receiver,
                                        &mut method_slots,
                                    ) {
                                        *ip = site.pc;
                                        break 'pass Ok(());
                                    }
                                    let entry = &methods[index][entry];
                                    (entry.leaf(), entry.has_fields.then_some(&method_slots))
                                }
                            };
                            let leaf_weight = leaf.body.weight;
                            // A method's leaf weight is not in the bound: in a
                            // fast segment it must fit beside what remains.
                            if fast
                                && site.callee.dynamic()
                                && leaf_weight != 0
                                && leaf_weight.saturating_add(weight)
                                    >= self.cost_flush_at().saturating_sub(*local_cost)
                            {
                                // Nothing of the call is done yet, and `CALL` is
                                // free: make it again, per-operation.
                                slow = true;
                                k -= 1;
                                continue 'segment;
                            }
                            // The call's admission safepoint, and in a
                            // per-operation segment a leaf charge that could be
                            // refused, hand the call to canonical dispatch.
                            if self.heap.gc_poll_requested()
                                || (!fast && !self.cost.admits_charges(*local_cost, leaf_weight))
                            {
                                *ip = site.pc;
                                break 'pass Ok(());
                            }
                            // Leaf parameter `p` is argument `p`; the entry or
                            // the admission checked every guarded one is a
                            // number passed.
                            let argument = |register: u16| x[at(site.args[usize::from(register)])];
                            let (result, path) = match fields {
                                None if leaf.body.control => leaf.eval::<true>(argument),
                                None => leaf.eval::<false>(argument),
                                Some(fields) => self.run_leaf_fields(leaf, fields, argument),
                            };
                            // The leaf's whole weight bounded the segment or
                            // was admitted; the charges made are the path's.
                            if fast {
                                *local_cost += leaf.body.path_weight(path);
                                #[cfg(feature = "cost-check-interval")]
                                if leaf_weight != 0 {
                                    leaf.body.record_charges(path);
                                }
                            } else {
                                // bounded: one leaf body's charge sites plus its tail charge
                                for (cursor, charge) in leaf.body.path_charges(path) {
                                    if charge != 0
                                        && let Err(error) =
                                            self.charge_dispatch(local_cost, cursor, charge)
                                    {
                                        break 'pass Err(error);
                                    }
                                }
                            }
                            // Without a result `above` holds `r`'s own slot, so
                            // the write is dropped again.
                            x[at(op.a)] = result;
                            written = (written | 1 << op.a) & !site.above;
                            published &= !site.above;
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
                break;
            }
            let fast =
                !slow && (weight == 0 || weight < self.cost_flush_at().saturating_sub(*local_cost));
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
            first_pass = false;
        };
        self.publish_mirror(body, base, &x, written);
        // Last, so no published number overwrites it.
        if CALLS && let Some(index) = loaded {
            let function = base + usize::from(body.calls[index].register);
            let (callee, receiver) = match &callees[index] {
                Some(resolved) => (resolved.callee, resolved.receiver),
                None => {
                    let (method, receiver, _) = current[index];
                    (method, receiver.map_or(Val::Nil, Val::Obj))
                }
            };
            self.stack[function] = callee;
            if body.calls[index].callee.method() {
                self.stack[function + 1] = receiver;
            }
        }
        result
    }

    /// Read each record site's key into `keys`. `false` declines the region:
    /// a stored key is `__index` (a live overwrite of it would need route
    /// retirement), or a stored key is the key of a field the region reads
    /// once at entry, which the store could change. Off in the forced-miss
    /// arm of the inline-cache differential, as field leaves are, so that
    /// arm runs canonical dispatch.
    fn resolve_record_keys(
        &self,
        body: &Body,
        frame: &Frame,
        keys: &mut [Val; MAX_RECORD_SITES],
    ) -> bool {
        if body.record_sites.is_empty() {
            return true;
        }
        #[cfg(feature = "disable-inline-caches")]
        if super::caches::hits_disabled() {
            return false;
        }
        let index_key = self.meta_names.get(MetaName::Index);
        // bounded: at most MAX_RECORD_SITES store sites
        for (site, key) in body.record_sites.iter().zip(keys.iter_mut()) {
            *key = frame.literal(site.key);
            if !site.store {
                continue;
            }
            if *key == index_key {
                return false;
            }
            // bounded: at most MIRROR_SLOTS constants per store site
            let hoisted = body.constants.iter().any(|&(_, constant)| {
                matches!(constant, Constant::Field { key: read, .. } if frame.literal(read) == *key)
            });
            if hoisted {
                return false;
            }
        }
        true
    }

    /// The live slot of `key` in record `table` and its value, through the
    /// site's last slot `found`: the same slot while the table and its
    /// version are unchanged, else the same position if it holds `key` live
    /// (in this table or one of the same layout), else a lookup. `None` when
    /// the key is not live in `table`.
    #[inline(always)]
    fn record_slot(
        &self,
        table: ObjectPtr,
        key: Val,
        found: &mut Option<(ObjectPtr, SlotRef)>,
    ) -> Option<(SlotRef, Val)> {
        let tbl = self.heap.as_table_ref(table)?;
        if let Some((last, slot)) = *found {
            if last == table
                && let Some(value) = tbl.read_slot(slot)
            {
                return Some((slot, value));
            }
            if let Some((slot, value)) = tbl.renew_slot(slot, key) {
                *found = Some((table, slot));
                return Some((slot, value));
            }
        }
        let (slot, value) = tbl.find_slot(&key)?;
        *found = Some((table, slot));
        Some((slot, value))
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
        // bounded: at most MAX_CALLS call sites
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
                // Looked up per pass (`State::region_method_entry`); the
                // checks above hold for the whole run, since nothing the
                // region runs captures a register or changes the depth.
                Callee::Dynamic { .. } => continue,
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

    /// The keys some resolution of the region relies on: every method key
    /// (of an invariant receiver or of a record), every callee's global
    /// name and every field the region reads at entry. Keys are interned,
    /// so equal keys are equal values.
    fn relied_keys(&self, body: &Body, frame: &Frame) -> Vec<Val> {
        body.calls
            .iter()
            .filter_map(|site| match site.callee {
                Callee::Global { name, .. } => Some(name),
                Callee::Method { key, .. } | Callee::Dynamic { key, .. } => Some(key),
                Callee::Upvalue(_) => None,
            })
            .chain(
                body.constants
                    .iter()
                    .filter_map(|&(_, constant)| match constant {
                        Constant::Field { key, .. } => Some(key),
                        Constant::Number(_) | Constant::Upvalue(_) => None,
                    }),
            )
            .map(|key| frame.literal(key))
            .collect()
    }

    /// Whether no store of the region (a leaf of `callees`, a record
    /// store) is under a key some resolution of the region relies on
    /// (`State::relied_keys`). A method leaf admitted mid-run is checked
    /// when it is admitted (`State::admit_method`).
    fn region_keys_disjoint(
        &self,
        body: &Body,
        frame: &Frame,
        callees: &[Option<Resolved>; MAX_CALLS],
    ) -> bool {
        let relied = self.relied_keys(body, frame);
        // bounded: at most MAX_CALLS sites of MAX_FIELD_SITES each, and
        // MAX_RECORD_SITES record sites, against the relied keys
        let mut leaves = callees.iter().flatten().flat_map(|resolved| {
            let runtime = &resolved.runtime;
            resolved
                .leaf()
                .sites
                .iter()
                .filter(|site| site.store)
                .map(|site| runtime.literals[usize::from(site.key)])
        });
        let mut records = body
            .record_sites
            .iter()
            .filter(|site| site.store)
            .map(|site| frame.literal(site.key));
        leaves.all(|key| !relied.contains(&key)) && records.all(|key| !relied.contains(&key))
    }

    /// The cache entry of the method `key` of record `table` at `site`,
    /// filled on a miss. `None` exits at the lookup: the record holds the
    /// key itself, has no metatable, the site's cache is full, the route
    /// would run a handler (`State::region_method`), or the method cannot
    /// run here (`State::admit_method`).
    fn region_method_entry(
        &self,
        table: ObjectPtr,
        key: Val,
        entries: &mut Vec<RegionMethod>,
        site: &CallSite,
        base: usize,
        relied: &[Val],
    ) -> Option<usize> {
        let tbl = self.heap.as_table_ref(table)?;
        if !matches!(tbl.get(&key), Val::Nil) {
            return None;
        }
        let meta = tbl.get_metatable()?;
        if let Some(index) = entries.iter().position(|entry| entry.meta == meta) {
            return Some(index);
        }
        if entries.len() >= MAX_METHOD_CLASSES {
            return None;
        }
        let method = self.region_method(Val::Obj(table), key)?;
        entries.push(self.admit_method(meta, method, site, base, relied)?);
        Some(entries.len() - 1)
    }

    /// Admit `method`, found through metatable `meta`, for method calls at
    /// `site` for the rest of the run: what `State::resolve_callees` checks
    /// of a static method (a Lua closure with a leaf plan, a result when the
    /// call keeps one, parameter reads that are number arguments, the frame
    /// reservation), its stores under neither `__index` nor a key in
    /// `relied`, and its upvalue field sites resolved to live slots once (an
    /// upvalue open on this frame, which the region writes, refuses). Its
    /// receiver sites are found per pass (`State::method_fields`).
    fn admit_method(
        &self,
        meta: ObjectPtr,
        method: Val,
        site: &CallSite,
        base: usize,
        relied: &[Val],
    ) -> Option<RegionMethod> {
        let Val::Obj(ptr) = method else {
            return None;
        };
        let closure = self.heap.as_lua_function_ref(ptr)?;
        let runtime = &closure.runtime;
        let leaf = runtime.numeric_leaf.as_ref()?;
        if site.results == 1 && leaf.result.is_none() {
            return None;
        }
        let arguments = site.args.len();
        let parameters_are_arguments =
            leaf.body
                .registers
                .iter()
                .enumerate()
                .all(|(slot, &register)| {
                    let register = usize::from(register);
                    leaf.body.guards & (1 << slot) == 0 || (register >= 1 && register < arguments)
                });
        let function = base + usize::from(site.register);
        if !parameters_are_arguments
            || function + 1 + leaf.max_stack + FRAME_HEADROOM > MAX_STACK_SIZE
        {
            return None;
        }
        let index_key = self.meta_names.get(MetaName::Index);
        let mut fields: FieldSlots = [None; MAX_FIELD_SITES];
        let mut keys = [Val::Nil; MAX_FIELD_SITES];
        // bounded: at most MAX_FIELD_SITES field sites
        for ((field, slot_key), site) in fields.iter_mut().zip(&mut keys).zip(leaf.sites.iter()) {
            let key = runtime.literals[usize::from(site.key)];
            *slot_key = key;
            if site.store && (key == index_key || relied.contains(&key)) {
                return None;
            }
            match site.source {
                TableSource::Param(0) => {}
                TableSource::Param(_) => return None,
                TableSource::Upvalue(index) => {
                    let &uv_ref = closure.upvalues.as_slice().get(usize::from(index))?;
                    let value = match self.upvalue_pool.get(uv_ref) {
                        Upvalue::Open(stack_idx) if *stack_idx < base => self.stack[*stack_idx],
                        Upvalue::Open(_) => return None,
                        Upvalue::Closed(value) => *value,
                    };
                    let Val::Obj(table) = value else {
                        return None;
                    };
                    let (slot, value) = self.heap.as_table_ref(table)?.find_slot(&key)?;
                    if site.number && !matches!(value, Val::Num(_)) {
                        return None;
                    }
                    *field = Some((table, slot));
                }
            }
        }
        Some(RegionMethod {
            meta,
            method,
            runtime: Arc::clone(runtime),
            fields,
            keys,
            hints: [None; MAX_FIELD_SITES],
            has_fields: !leaf.sites.is_empty(),
        })
    }

    /// Resolve the field sites of `entry`'s leaf for a call on record
    /// `receiver` into `out`: its upvalue sites as admitted, its receiver
    /// sites as record sites are (`State::record_slot`). `false` when a
    /// receiver key is not live in the record or a read one does not hold a
    /// number: the call then runs canonically, with nothing done.
    fn method_fields(
        &self,
        entry: &mut RegionMethod,
        receiver: ObjectPtr,
        out: &mut FieldSlots,
    ) -> bool {
        let RegionMethod {
            runtime,
            fields,
            keys,
            hints,
            ..
        } = entry;
        let leaf = runtime
            .numeric_leaf
            .as_ref()
            .expect("admitted with a leaf plan");
        // bounded: at most MAX_FIELD_SITES field sites
        for (index, site) in leaf.sites.iter().enumerate() {
            out[index] = match site.source {
                TableSource::Param(_) => {
                    let Some((slot, value)) =
                        self.record_slot(receiver, keys[index], &mut hints[index])
                    else {
                        return false;
                    };
                    if site.number && !matches!(value, Val::Num(_)) {
                        return false;
                    }
                    Some((receiver, slot))
                }
                TableSource::Upvalue(_) => fields[index],
            };
        }
        true
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
        // bounded: at most MIRROR_SLOTS registers
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                match self.stack[base + usize::from(register)] {
                    Val::Num(n) => x[slot] = n,
                    _ => return false,
                }
            }
        }
        // bounded: at most MIRROR_SLOTS constants
        for &(slot, constant) in &body.constants {
            x[at(slot)] = match constant {
                Constant::Number(value) => value,
                Constant::Upvalue(index) => match self.upvalue_value(frame, index) {
                    Val::Num(n) => n,
                    _ => return false,
                },
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
        // bounded: at most MIRROR_SLOTS registers
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
    ///   leaves the batch where per-instruction charging would. A body with
    ///   forward branches is bounded by the sum over all its operations and
    ///   adds the charges of the operations it ran (`Body::path_weight`).
    ///
    /// Every field site is resolved on every path, so a call whose skipped
    /// path names a field the receiver lacks declines although canonical
    /// execution would not read it. A truth test reads the site's live
    /// value, so a store before it, through any receiver, is seen; its
    /// register holds no number afterwards and nothing reads it as one
    /// (`Layout::truth_test`).
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
        // bounded: at most MIRROR_SLOTS registers
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
        // bounded: at most MIRROR_SLOTS constants
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                x[at(slot)] = value;
            }
        }
        // The admission test above bounds every path by `body.weight`; the
        // charges added are those of the path run, which per-instruction
        // dispatch would make with no settlement between them.
        let (result, weight) = if leaf.sites.is_empty() {
            let path = if body.control {
                run_pure::<true>(body, &mut x)
            } else {
                run_pure::<false>(body, &mut x)
            };
            #[cfg(feature = "cost-check-interval")]
            if body.weight != 0 {
                body.record_charges(path);
            }
            (
                leaf.result.map(|result| x[at(result)]),
                body.path_weight(path),
            )
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
            let path = self.run_fields(&leaf.body, &fields, &mut x);
            #[cfg(feature = "cost-check-interval")]
            if leaf.body.weight != 0 {
                leaf.body.record_charges(path);
            }
            (
                leaf.result.map(|result| x[at(result)]),
                leaf.body.path_weight(path),
            )
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
        // bounded: at most MAX_FIELD_SITES field sites
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
    /// declines such a leaf at entry), and the operations it ran.
    #[inline(never)]
    fn run_leaf_fields(
        &mut self,
        leaf: &NumericLeaf,
        fields: &[Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
        argument: impl Fn(u16) -> f64,
    ) -> (f64, LeafPath) {
        let body = &leaf.body;
        let mut y = [0.0f64; MIRROR_SLOTS];
        // bounded: at most MIRROR_SLOTS registers
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                y[slot] = argument(register);
            }
        }
        // bounded: at most MIRROR_SLOTS constants
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                y[at(slot)] = value;
            }
        }
        let path = self.run_fields(body, fields, &mut y);
        (leaf.result.map_or(0.0, |result| y[at(result)]), path)
    }

    /// Run a field leaf's body over `x` with its sites resolved to
    /// `fields`, in program order (by index when it branches, forward
    /// only): each read reloads its slot, each store overwrites it with a
    /// number, each truth test reads the live value. Returns the operations
    /// run.
    #[inline(always)]
    fn run_fields(
        &mut self,
        body: &Body,
        fields: &[Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
        x: &mut [f64; MIRROR_SLOTS],
    ) -> LeafPath {
        if body.control {
            self.run_field_ops::<true>(body, fields, x)
        } else {
            self.run_field_ops::<false>(body, fields, x)
        }
    }

    /// `run_fields`, compiled apart for bodies with branches (`CONTROL`),
    /// so a straight-line body keeps its loop free of the path bits and
    /// the branch arms.
    fn run_field_ops<const CONTROL: bool>(
        &mut self,
        body: &Body,
        fields: &[Option<(ObjectPtr, SlotRef)>; MAX_FIELD_SITES],
        x: &mut [f64; MIRROR_SLOTS],
    ) -> LeafPath {
        let mut path = if CONTROL { 0 } else { FULL_PATH };
        let mut k = 0;
        // bounded: branches, jumps and tests only move the index forward
        while let Some(op) = body.ops.get(k) {
            if CONTROL {
                path |= 1 << (k % MAX_CONTROL_LEAF_OPS);
            }
            k += 1;
            match op.kind {
                OpKind::Test(truthy) if CONTROL => {
                    let (table, slot) =
                        fields[usize::from(op.b)].expect("resolve_sites resolved every site");
                    // A store writes a number, so a tested slot stays live,
                    // and a store before the test (through any receiver)
                    // is seen, as canonical execution would see it.
                    let value = self
                        .heap
                        .as_table_ref(table)
                        .and_then(|tbl| tbl.read_slot(slot))
                        .expect("a resolved site stays live");
                    if value.truthy() == truthy {
                        k = usize::from(op.a);
                    }
                }
                OpKind::Branch(compare) if CONTROL => {
                    if !holds(compare, x[at(op.b)], x[at(op.c)]) {
                        k = usize::from(op.a);
                    }
                }
                OpKind::Jump if CONTROL => k = usize::from(op.a),
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
        path
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
        // Branches and jumps by `run_control_region` and the leaf
        // runners, truth tests by a field leaf, indexed reads and records
        // only by `run_control_region`.
        OpKind::Copy
        | OpKind::Load
        | OpKind::Call
        | OpKind::Read
        | OpKind::Store
        | OpKind::Test(_)
        | OpKind::Branch(_)
        | OpKind::Jump
        | OpKind::Index
        | OpKind::Record
        | OpKind::RecordRead
        | OpKind::RecordStore
        | OpKind::Prep
        | OpKind::Step => lhs,
    }
}

impl NumericLeaf {
    /// The leaf's result with `argument(register)` as each parameter it
    /// reads, which the caller has checked is a number it passes, and the
    /// operations it ran.
    #[inline(always)]
    /// `CONTROL` must be the body's `control`: compiled apart, so a
    /// straight-line body keeps its loop free of the branch arms.
    fn eval<const CONTROL: bool>(&self, argument: impl Fn(u16) -> f64) -> (f64, LeafPath) {
        debug_assert_eq!(CONTROL, self.body.control);
        let body = &self.body;
        let mut y = [0.0f64; MIRROR_SLOTS];
        // bounded: at most MIRROR_SLOTS registers
        for (slot, &register) in body.registers.iter().enumerate() {
            if body.guards & (1 << slot) != 0 {
                y[slot] = argument(register);
            }
        }
        // bounded: at most MIRROR_SLOTS constants
        for &(slot, constant) in &body.constants {
            if let Constant::Number(value) = constant {
                y[at(slot)] = value;
            }
        }
        let path = run_pure::<CONTROL>(body, &mut y);
        // A call keeping a result declines a leaf that returns nothing.
        (self.result.map_or(0.0, |result| y[at(result)]), path)
    }
}

/// Run the operations of a leaf without field sites over `x`: in order
/// for a straight-line body, else by index, a branch or jump moving the
/// index forward. Returns the operations run. `CONTROL` must be the body's
/// `control`.
#[inline(always)]
fn run_pure<const CONTROL: bool>(body: &Body, x: &mut [f64; MIRROR_SLOTS]) -> LeafPath {
    if !CONTROL {
        // bounded: one pass over the leaf body's straight-line operations
        for op in &body.ops {
            x[at(op.a)] = eval(op, x);
        }
        return FULL_PATH;
    }
    let mut path = 0;
    let mut k = 0;
    // bounded: branches and jumps only move the index forward
    while let Some(op) = body.ops.get(k) {
        path |= 1 << (k % MAX_CONTROL_LEAF_OPS);
        k += 1;
        match op.kind {
            OpKind::Branch(compare) => {
                if !holds(compare, x[at(op.b)], x[at(op.c)]) {
                    k = usize::from(op.a);
                }
            }
            OpKind::Jump => k = usize::from(op.a),
            _ => x[at(op.a)] = eval(op, x),
        }
    }
    path
}

/// The charges of a straight-line pass's operations before operation
/// `end`, the leaves of the calls among them included; every such leaf is
/// straight-line (`State::run_region`), so its whole weight is its charge.
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

/// Record the positive charges of a straight-line pass's operations before
/// `end`, each call's leaf charges after the call's own, then `FOR_LOOP`'s
/// if `tail`, in the charge-site trace, as per-instruction dispatch would.
#[cfg(feature = "cost-check-interval")]
fn record_pass<'a>(body: &Body, leaf: &impl Fn(usize) -> &'a NumericLeaf, end: usize, tail: bool) {
    // bounded: at most one pass over the body's operation and charge lists
    for (op, &(cursor, weight)) in body.ops.iter().zip(&body.charges).take(end) {
        if weight != 0 {
            crate::cost_meter::interval_test::record(cursor, weight);
        }
        if let OpKind::Call = op.kind {
            leaf(usize::from(op.b)).body.record_charges(FULL_PATH);
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
            // A receiver the body writes.
            "local function f(t) local s = 0 for i = 1, 9 do s = s + t[i]; t = i end end",
            // A string comparison and a truth test are not number branches.
            "local function f(t) local s = 0 for i = 1, 9 do if s == 'x' then s = 1 end end end",
            "local function f(t) local s = 0 for i = 1, 9 do if s then s = 1 end end end",
        ] {
            assert_eq!(regions(source), 0, "{source}");
        }
    }

    #[test]
    fn regions_take_records_read_from_invariant_tables() {
        for source in [
            "local function f(us) for i = 1, 9 do local u = us[i] u.x = u.x + u.speed end end",
            "local function f(us) local s = 0 for i = 1, 9 do s = s + us[i].x end return s end",
            "local us = {} local function f() for i = 1, 9 do local u = us[i] if u.x > 2 then u.x = 0 end end end",
            "local function f(us) for i = 1, 9 do local u = us[i] u.hp = 1 end end",
        ] {
            assert_eq!(regions(source), 1, "{source}");
        }
        for source in [
            // A record on one path only; a record used as a number; a
            // record carried into the next pass; a store of a record.
            "local function f(us, v) for i = 1, 9 do if i > 3 then v = us[i] end v.x = 1 end end",
            "local function f(us) local s = 0 for i = 1, 9 do local u = us[i] u.x = 1 s = s + u end end",
            "local function f(us, u) local s = 0 for i = 1, 9 do s = s + u u = us[i] u.x = 1 end end",
            "local function f(us) for i = 1, 9 do local u = us[i] u.x = u end end",
            // A record replacing an invariant receiver.
            "local function f(us, o) local s = 0 for i = 1, 9 do s = s + o.y o = us[i] o.x = 1 end end",
        ] {
            assert_eq!(regions(source), 0, "{source}");
        }
    }

    #[test]
    fn regions_take_nested_loops_and_method_calls_on_records() {
        // A nested loop runs inside its enclosing region, and is a region of
        // its own too; a number upvalue read inside a body is read at entry.
        // Under fault injection every load refuses, so the outer loop, which
        // loads `n`, is no region there.
        let taken = usize::from(!cfg!(feature = "fault-injection"));
        for (source, expected) in [
            (
                "local function f() local s = 0 for i = 1, 9 do for j = 1, i do s = s + j end end end",
                2,
            ),
            (
                "local n = 5 local function f() local s = 0 for i = 1, 9 do for j = i, n do if j > 2 then s = s + j end end s = s - i end end",
                1 + taken,
            ),
        ] {
            assert_eq!(regions(source), expected, "{source}");
        }
        // A method call on a record read per pass, beside a branch and
        // record stores; a branch beside a call.
        for source in [
            "local function f(us) local s = 0 for i = 1, 9 do s = s + us[i]:m() end return s end",
            "local function f(us) for i = 1, 9 do local u = us[i] u:m(i % 5) u.x = u.x + u.speed end end",
            "local function f() local s = 0 for i = 1, 9 do if s > 1 then s = g(s) end end end",
        ] {
            assert_eq!(regions(source), taken, "{source}");
        }
        // Number upvalues loaded back to back, as both operands of one
        // expression and beside a call; a captured callee whose call comes
        // after another load's still refuses.
        for (source, expected) in [
            (
                "local a, b = 1, 2 local function f() local s = 0 for i = 1, 9 do s = a + (b - s) end return s end",
                taken,
            ),
            (
                "local a, b = 1, 2 local function f() local s = 0 for i = 1, 9 do s = a - (b - g(s)) end return s end",
                taken,
            ),
            (
                "local g, h = 1, 2 local function f() local s = 0 for i = 1, 9 do s = g(h(s)) end return s end",
                0,
            ),
        ] {
            assert_eq!(regions(source), expected, "{source}");
        }
        // A record copied as a number, and a record carried into the
        // next pass through a nested loop's head.
        for source in [
            "local function f(us) for i = 1, 9 do local u = us[i] local v = u u:m() end end",
            "local function f(us, u) local s = 0 for i = 1, 9 do for j = 1, 2 do s = s + u u = us[j] u.x = 1 end end end",
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
        // Forward branches: on a field's truthiness, on number comparisons,
        // with joins reaching the first `RETURN`.
        assert!(leaf(
            "local function f(self, dt) if self.alive then self.x = self.x + self.vx * dt self.age = self.age + 1 end end"
        ));
        assert!(leaf(
            "local function f(self) if not self.on then self.n = self.n + 1 end end"
        ));
        assert!(leaf(
            "local function f(x) local y = 0 if x > 1 then y = x else y = -x end return y end"
        ));
        for source in [
            // The tested register holds no number past the test.
            "local function f(self) local a = self.on if a then end return a end",
            // A table defined on one path only.
            "local u = {} local function f(x) if x > 1 then u.n = 2 end end",
            // A return past the first.
            "local function f(x) if x > 1 then return 1 end return 2 end",
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
