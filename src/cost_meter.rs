//! Runtime cost accounting independent of VM state.

use crate::cost_model::{Charge, CostModel, Costing};

/// Version of the cost model: what `cost_used` a given source produces.
///
/// The version covers everything that decides the cost of identical source
/// under any [`CostModel`]: the units each charge site measures, which charge
/// a piece of work is attributed to, how a charge is admitted against a budget
/// (atomic, or per unit), the default weights, and compiler emission (which
/// opcodes a source compiles to). Moving work from one charge to another
/// needs a bump even when the default costs do not change, because a
/// non-default model prices the two charges differently. A cost fingerprint
/// is this version plus the State's costing (`CostModel::iter` for a metered
/// one); hosts may key stored fingerprints on it. Snapshots require an exact
/// match. `tests/integration/cost/cost_golden.rs` checks a
/// versioned source and native-call fixture against an append-only
/// (version, fingerprint) history, so changing a pinned cost requires a bump.
///
/// Version 1 was the original implicit model. Version 2 charges native string
/// and pattern work in addition to bytecode instructions. Version 3 folds
/// literal arithmetic at compile time (folded operations no longer charge) and
/// charges `string.gmatch` pattern compilation once per iterator rather than
/// once per compilation-cache miss. Version 4 stops folding `^`, so literal
/// exponentiation charges like any other `^` (its result then came from the
/// host libm, which must not be baked into portable bytecode; `^` now uses the
/// deterministic `crmath::pow` and stays unfolded to keep this charge). Version 5 charges
/// opcode work still pending in the dispatch loop's batch when a runtime error
/// (other than budget exhaustion) ends a frame, so `cost_used` after a failed
/// callback counts every executed costed opcode. The table library also runs
/// sequence-library metamethods with their normal execution costs, and
/// output-byte charges change through unsigned NaN formatting and decimal
/// integral `%q` output. Version 6 compiles a `string.gmatch` pattern's
/// leading `^` as a literal byte instead of rewriting it to `%^`, so the
/// iterator's first-call compilation charge for such a pattern is one unit
/// lower. Version 7 is the register VM: a Lua call reserves its whole frame
/// when it is admitted, so a run that reaches `MAX_STACK_SIZE` fails at a
/// different point (and with a different charged prefix) than before. Runs
/// that never reach the stack cap charge exactly as under version 6. Version 8
/// charges one unit for each `rawset` call, like a `TableWrite` opcode, and
/// `string.gsub` rejects an invalid replacement argument before charging for
/// its subject and pattern. Version 9 adopts one rule for string costs: a
/// string library call charges 1 (`string.format` 1 per directive, at least
/// 1), pattern matching adds its matcher steps and substring-scan
/// comparisons, and no byte read or produced is charged; `table.concat`
/// charges a flat 1 admitted after its argument checks. The subject, pattern,
/// replacement-template, capture, output and separator byte charges are gone,
/// and so is `string.gmatch`'s first-call pattern charge.
pub const COST_MODEL_VERSION: u16 = 9;

/// Debug-only dispatch batching controls and charged-opcode observations.
#[cfg(feature = "cost-check-interval")]
pub(crate) mod interval_test {
    use std::cell::{Cell, RefCell};

    // process-global: thread-local test controls isolate concurrent test runs.
    // Outside a `with_cost_check_interval` scope the production interval
    // applies, so the feature alone changes no test's behavior.
    thread_local! {
        static INTERVAL: Cell<u64> =
            const { Cell::new(crate::limits::tuning::COST_CHECK_INTERVAL) };
        static PREFIX: RefCell<Option<Vec<(usize, u64)>>> = const { RefCell::new(None) };
    }

    pub(crate) fn interval() -> u64 {
        INTERVAL.get()
    }

    pub(crate) fn record(ip: usize, cost: u64) {
        PREFIX.with_borrow_mut(|prefix| {
            if let Some(prefix) = prefix {
                prefix.push((ip, cost));
            }
        });
    }

    struct Restore {
        interval: u64,
        prefix: Option<Vec<(usize, u64)>>,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            INTERVAL.set(self.interval);
            PREFIX.set(self.prefix.take());
        }
    }

    /// Runs test work with a batching interval, returning its accepted opcode
    /// charge sites (instruction cursor and units) in execution order.
    /// Thread-local controls are restored on return or panic, including nesting.
    #[doc(hidden)]
    pub fn with_cost_check_interval<T>(
        interval: u64,
        work: impl FnOnce() -> T,
    ) -> (T, Vec<(usize, u64)>) {
        assert!(interval > 0, "a batching interval must be positive");
        let _restore = Restore {
            interval: INTERVAL.replace(interval),
            prefix: PREFIX.replace(Some(Vec::new())),
        };
        let result = work();
        let prefix = PREFIX
            .take()
            .expect("the observation scope owns its prefix");
        (result, prefix)
    }
}

/// The opcode batching interval: the production constant, or under the
/// debug `cost-check-interval` feature the current scope's override, read
/// when a [`CostState`] recomputes its flush threshold.
#[inline(always)]
fn batch_interval() -> u64 {
    #[cfg(not(feature = "cost-check-interval"))]
    {
        crate::limits::tuning::COST_CHECK_INTERVAL
    }
    #[cfg(feature = "cost-check-interval")]
    {
        interval_test::interval()
    }
}

/// What a finite budget still admits: `limit - used`, clamped at 0. A
/// positive charge is admitted exactly when this is above 0.
#[inline(always)]
fn room(limit: i64, used: u64) -> u64 {
    u64::try_from(limit).map_or(0, |limit| limit.saturating_sub(used))
}

/// The pending opcode cost at which dispatch settles its batch: the interval,
/// or for a finite budget the room left if that is smaller, so the batch is
/// settled before it would reach the budget edge. Zero once the budget is
/// spent, so the next positive charge goes straight to admission and fails.
#[inline(always)]
fn flush_threshold(limit: Option<i64>, used: u64) -> u64 {
    let interval = batch_interval();
    limit.map_or(interval, |limit| interval.min(room(limit, used)))
}

/// Owned runtime accounting: the State's fixed costing, and its counters with
/// one representation of an optional budget.
///
/// `used` is the one authoritative counter. A finite budget stores only its
/// limit; what remains is derived as `limit - used` (saturating), which is
/// exactly what a separately decremented remainder would hold, saturation at
/// `u64::MAX` used and `i64::MIN` remaining included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CostState {
    costing: Costing,
    /// The weights charge sites read: the metered model's, or every weight 0
    /// when costing is off, so a dispatch arm skips its charge either way.
    effective: CostModel,
    /// The finite budget's limit. Always `None` when costing is off.
    limit: Option<i64>,
    /// Always 0 when costing is off.
    used: u64,
    /// The dispatch loop's one settlement test reads this (`pending >=
    /// flush_at`), so every change to `limit` or `used` recomputes it:
    /// construction, `set_budget`, snapshot load and every finite
    /// [`CostMeter`] charge. Count-only and off thresholds do not depend on
    /// `used`, so counting charges leave it alone. Under the debug
    /// `cost-check-interval` feature it captures the interval in scope when
    /// it was computed, so a test builds and runs its State inside one scope.
    flush_at: u64,
}

impl Default for CostState {
    fn default() -> Self {
        Self::new(Costing::default())
    }
}

impl CostState {
    /// Fresh counters with no budget, under `costing`.
    pub(crate) fn new(costing: Costing) -> Self {
        let effective = match &costing {
            Costing::Metered(model) => model.clone(),
            Costing::Off => CostModel::zero(),
        };
        Self {
            costing,
            effective,
            limit: None,
            used: 0,
            flush_at: flush_threshold(None, 0),
        }
    }

    pub(crate) fn costing(&self) -> &Costing {
        &self.costing
    }

    /// The weight charge sites apply to one unit of `charge`: 0 when off.
    #[inline(always)]
    pub(crate) fn weight(&self, charge: Charge) -> u64 {
        u64::from(self.effective.weight(charge))
    }

    /// The cost of `units` units of `charge`, saturating: 0 when off.
    #[inline(always)]
    pub(crate) fn cost(&self, charge: Charge, units: u64) -> u64 {
        self.effective.cost(charge, units)
    }

    /// The pending opcode cost at which dispatch must settle its batch.
    #[inline(always)]
    pub(crate) fn flush_at(&self) -> u64 {
        self.flush_at
    }

    /// A meter whose unit costs the weight of `charge`.
    pub(crate) fn charge_meter(&mut self, charge: Charge) -> CostMeter<'_> {
        let unit = self.weight(charge);
        self.meter().with_unit(unit)
    }

    /// Sets a finite budget and zeroes `used`. Refused, changing nothing,
    /// when costing is off.
    pub(crate) fn set_budget(&mut self, limit: i64) -> bool {
        if matches!(self.costing, Costing::Off) {
            return false;
        }
        self.limit = Some(limit);
        self.used = 0;
        self.flush_at = flush_threshold(self.limit, self.used);
        true
    }

    pub(crate) fn used(&self) -> u64 {
        self.used
    }

    pub(crate) fn remaining(&self) -> i64 {
        self.limit
            .map_or(i64::MAX, |limit| limit.saturating_sub_unsigned(self.used))
    }

    pub(crate) fn limit(&self) -> i64 {
        self.limit.unwrap_or(i64::MAX)
    }

    /// A meter charging raw cost (unit cost 1).
    pub(crate) fn meter(&mut self) -> CostMeter<'_> {
        if matches!(self.costing, Costing::Off) {
            return CostMeter::Off;
        }
        match self.limit {
            Some(limit) => CostMeter::finite_budget(limit, &mut self.used, &mut self.flush_at),
            None => CostMeter::count_only(&mut self.used),
        }
    }

    // Retain the existing snapshot layout, including canonical unbounded sentinels.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_fields(&self) -> (i64, i64, bool, u64) {
        (
            self.remaining(),
            self.limit(),
            self.limit.is_some(),
            self.used,
        )
    }

    /// Rebuild the accounting from snapshot fields, rejecting any combination
    /// the runtime cannot reach: an unconfigured meter must carry the
    /// unbounded sentinels, a configured one must satisfy
    /// `remaining == limit - used` (saturating) with no use against a budget
    /// that started exhausted, and costing that is off has neither a budget
    /// nor any use. The saved `remaining` is validated, then dropped: it is
    /// derived from `limit` and `used`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn from_snapshot(
        costing: Costing,
        remaining: i64,
        limit: i64,
        configured: bool,
        used: u64,
    ) -> Option<Self> {
        let limit = if configured {
            if (limit <= 0 && used != 0) || remaining != limit.saturating_sub_unsigned(used) {
                return None;
            }
            Some(limit)
        } else {
            if remaining != i64::MAX || limit != i64::MAX {
                return None;
            }
            None
        };
        if matches!(costing, Costing::Off) && (limit.is_some() || used != 0) {
            return None;
        }
        let mut state = Self::new(costing);
        state.limit = limit;
        state.used = used;
        state.flush_at = flush_threshold(limit, used);
        Some(state)
    }
}

/// Tracks costs either without a budget or against a finite budget.
///
/// The meter deliberately borrows only counters. This keeps data-dependent
/// native work independent from [`crate::State`], so non-VM components can use
/// it without depending on the VM module.
///
/// `unit` is the cost of one unit of work in [`CostMeter::consume_units`]:
/// the weight of the charge the meter was made for (see
/// [`crate::cost_model`]). [`CostMeter::consume`] charges raw cost and ignores it.
pub(crate) enum CostMeter<'a> {
    /// Costing is off: admits everything and counts nothing.
    Off,
    /// Counts work while no budget has been configured.
    CountOnly { used: &'a mut u64, unit: u64 },
    /// Counts work and checks it against a configured budget, keeping the
    /// dispatch flush threshold in step with `used`.
    FiniteBudget {
        limit: i64,
        used: &'a mut u64,
        flush_at: &'a mut u64,
        unit: u64,
    },
}

impl<'a> CostMeter<'a> {
    /// Creates a meter that only records consumed cost, with unit cost 1.
    pub(crate) fn count_only(used: &'a mut u64) -> Self {
        Self::CountOnly { used, unit: 1 }
    }

    /// Creates a meter for an explicitly configured finite budget, with unit
    /// cost 1. Every admitted charge rewrites `flush_at` for the new `used`.
    pub(crate) fn finite_budget(limit: i64, used: &'a mut u64, flush_at: &'a mut u64) -> Self {
        Self::FiniteBudget {
            limit,
            used,
            flush_at,
            unit: 1,
        }
    }

    /// This meter with `unit` as the cost of one unit of work.
    pub(crate) fn with_unit(mut self, unit: u64) -> Self {
        match &mut self {
            Self::CountOnly { unit: slot, .. } | Self::FiniteBudget { unit: slot, .. } => {
                *slot = unit;
            }
            Self::Off => {}
        }
        self
    }

    /// Charges `n` units with the exact outcome of `n` consecutive
    /// `consume(unit)` calls: on a finite budget the charge stops at the
    /// first unit that starts with nothing remaining and reports blocked,
    /// leaving `used` exactly where the unit-at-a-time loop would have left
    /// it. This lets scans that charge per comparison batch their accounting
    /// without becoming observably different from the loop they replaced.
    #[inline]
    pub(crate) fn consume_units(&mut self, n: u64) -> bool {
        self.consume_units_admitted(n) == n
    }

    /// [`CostMeter::consume_units`], returning how many of the `n` units
    /// were admitted (all `n` unless the budget blocked one).
    #[inline]
    pub(crate) fn consume_units_admitted(&mut self, n: u64) -> u64 {
        match self {
            Self::Off => n,
            Self::CountOnly { used, unit } => {
                **used = (**used).saturating_add(n.saturating_mul(*unit));
                n
            }
            Self::FiniteBudget {
                limit,
                used,
                flush_at,
                unit,
            } => {
                if n == 0 || *unit == 0 {
                    return n;
                }
                let room = room(*limit, **used);
                if room == 0 {
                    return 0;
                }
                // Units that start with budget left: ceil(room / unit).
                let allowed = n.min(room.div_ceil(*unit));
                **used = (**used).saturating_add(allowed.saturating_mul(*unit));
                **flush_at = flush_threshold(Some(*limit), **used);
                allowed
            }
        }
    }

    /// How many further one-unit charges ([`CostMeter::consume_units`] of 1
    /// each) would be admitted: `u64::MAX` when nothing can block (off,
    /// count-only, a zero unit), otherwise the units that start with budget
    /// left, the crossing one included. A kernel that batches per-unit work
    /// reads this to stop exactly where the unit-at-a-time loop would.
    #[inline]
    pub(crate) fn admissible_units(&self) -> u64 {
        match self {
            Self::Off | Self::CountOnly { .. } => u64::MAX,
            Self::FiniteBudget {
                limit, used, unit, ..
            } => {
                if *unit == 0 {
                    return u64::MAX;
                }
                room(*limit, **used).div_ceil(*unit)
            }
        }
    }

    /// Charges `cost`, returning false when a positive charge is blocked.
    ///
    /// Like [`crate::State::consume_cost`], a charge that begins with a
    /// positive remaining budget may cross zero and still succeeds.
    #[inline(always)]
    pub(crate) fn consume(&mut self, cost: u64) -> bool {
        match self {
            Self::Off => true,
            Self::CountOnly { used, .. } => {
                **used = (**used).saturating_add(cost);
                true
            }
            Self::FiniteBudget {
                limit,
                used,
                flush_at,
                ..
            } => {
                if cost > 0 && room(*limit, **used) == 0 {
                    return false;
                }
                **used = (**used).saturating_add(cost);
                **flush_at = flush_threshold(Some(*limit), **used);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CostMeter, CostState};
    use crate::cost_model::{Charge, CostModel, Costing};

    #[test]
    fn off_admits_everything_and_counts_nothing() {
        let mut cost = CostState::new(Costing::Off);
        assert!(!cost.set_budget(10));
        assert!(cost.meter().consume(u64::MAX));
        assert_eq!(
            cost.charge_meter(Charge::TableMove)
                .consume_units_admitted(9),
            9
        );
        assert_eq!(cost.cost(Charge::Add, 5), 0);
        assert_eq!(cost.used(), 0);
        assert_eq!(cost.remaining(), i64::MAX);
    }

    #[test]
    fn metered_charge_meter_uses_the_model_weight() {
        let model = CostModel::default().with(Charge::StringFind, 3);
        let mut cost = CostState::new(Costing::Metered(model));
        assert!(cost.set_budget(7));
        assert_eq!(
            cost.charge_meter(Charge::StringFind)
                .consume_units_admitted(5),
            3
        );
        assert_eq!(cost.used(), 9);
        assert_eq!(cost.remaining(), -2);
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_meter_rejects_unreachable_counters() {
        for fields in [
            (0, 0, false, 0),
            (i64::MAX, 0, false, 0),
            (11, 10, true, 0),
            (9, 10, true, 0),
            (10, 10, true, 1),
            (-2, -1, true, 0),
            (-2, -1, true, 1),
            (-1, 0, true, 1),
        ] {
            assert!(
                CostState::from_snapshot(
                    Costing::default(),
                    fields.0,
                    fields.1,
                    fields.2,
                    fields.3
                )
                .is_none()
            );
        }
        // Costing that is off has neither a budget nor any use.
        for fields in [(5, 5, true, 0), (i64::MAX, i64::MAX, false, 1)] {
            assert!(
                CostState::from_snapshot(Costing::Off, fields.0, fields.1, fields.2, fields.3)
                    .is_none()
            );
        }
        assert!(CostState::from_snapshot(Costing::Off, i64::MAX, i64::MAX, false, 0).is_some());
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_meter_roundtrips_crossing_reset_and_saturation() {
        for limit in [
            None,
            Some(i64::MIN),
            Some(-1),
            Some(0),
            Some(1),
            Some(i64::MAX),
        ] {
            let mut cost = CostState::default();
            if let Some(limit) = limit {
                assert!(cost.set_budget(limit));
            }
            for charge in [0, 3, 0, u64::MAX, 1] {
                let accepted = cost.meter().consume(charge);
                let fields = cost.snapshot_fields();
                let mut restored = CostState::from_snapshot(
                    Costing::default(),
                    fields.0,
                    fields.1,
                    fields.2,
                    fields.3,
                )
                .expect("runtime counters must be restorable");
                assert_eq!(restored.snapshot_fields(), fields);
                assert_eq!(restored.meter().consume(1), cost.meter().consume(1));
                assert_eq!(restored.snapshot_fields(), cost.snapshot_fields());
                if !accepted {
                    assert!(cost.remaining() <= 0);
                }
            }
            assert!(cost.set_budget(7));
            assert_eq!(cost.snapshot_fields(), (7, 7, true, 0));
        }
    }

    /// Charge `n` units one at a time - the loop `consume_units` must match.
    fn naive_units(limit: i64, used: &mut u64, flush_at: &mut u64, n: u64, unit: u64) -> u64 {
        for admitted in 0..n {
            if !CostMeter::finite_budget(limit, used, flush_at).consume(unit) {
                return admitted;
            }
        }
        n
    }

    #[test]
    fn consume_units_matches_the_unit_at_a_time_loop_exactly() {
        for unit in [0u64, 1, 2, 3, 7] {
            for start in [-3i64, 0, 1, 4, 5, 6, 100] {
                for n in [0u64, 1, 4, 5, 6, 50] {
                    // `start` is what remains: `limit - used`.
                    let limit = start + 7;
                    // Both start from a consistent threshold, which a charge
                    // that changes nothing may leave untouched.
                    let flush = super::flush_threshold(Some(limit), 7);
                    let (mut used_a, mut flush_a) = (7u64, flush);
                    let (mut used_b, mut flush_b) = (7u64, flush);
                    let batched = CostMeter::finite_budget(limit, &mut used_a, &mut flush_a)
                        .with_unit(unit)
                        .consume_units_admitted(n);
                    let looped = naive_units(limit, &mut used_b, &mut flush_b, n, unit);
                    let at = format!("unit={unit} start={start} n={n}");
                    assert_eq!(batched, looped, "admitted mismatch at {at}");
                    assert_eq!(used_a, used_b, "used mismatch at {at}");
                    assert_eq!(flush_a, flush_b, "threshold mismatch at {at}");
                    // `admissible_units` predicts the loop's stopping point.
                    let (mut used_c, mut flush_c) = (7u64, flush);
                    let admissible = CostMeter::finite_budget(limit, &mut used_c, &mut flush_c)
                        .with_unit(unit)
                        .admissible_units();
                    assert_eq!(looped, n.min(admissible), "admissible mismatch at {at}");
                }
            }
        }
    }

    #[test]
    fn count_only_charges_saturate_used_cost() {
        use crate::vm::State;
        let mut state = State::new();
        state
            .consume_cost(u64::MAX - 1)
            .expect("count-only charges are never blocked");
        state
            .consume_cost(2)
            .expect("count-only charges saturate instead of overflowing");
        assert_eq!(state.cost_used(), u64::MAX);
    }

    #[test]
    fn consume_units_counts_without_a_budget() {
        let mut used = 3u64;
        assert!(CostMeter::count_only(&mut used).consume_units(9));
        assert_eq!(used, 12);
        assert!(
            CostMeter::count_only(&mut used)
                .with_unit(3)
                .consume_units(2)
        );
        assert_eq!(used, 18);
    }
}
