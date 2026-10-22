//! Runtime cost accounting independent of VM state.

/// Version of the cost model: what `cost_used` a given source produces.
///
/// The version covers everything that decides the cost of identical source -
/// per-opcode charges, native library charges, and compiler emission (which
/// opcodes a source compiles to). Hosts may key stored cost fingerprints on
/// it; snapshots require an exact match. `tests/cost_golden.rs` checks a
/// fixed corpus and fingerprints its sources, expected costs, and this version.
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
/// that never reach the stack cap charge exactly as under version 6.
pub const COST_MODEL_VERSION: u16 = 7;

/// Fingerprint of the fixed source and cost corpus in `tests/cost_golden.rs`,
/// including `COST_MODEL_VERSION`.
#[doc(hidden)]
pub const COST_MODEL_GOLDEN_FINGERPRINT: u64 = 0xf469_b9d2_69a4_f8de;

/// Owned runtime accounting, with one representation of an optional budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CostState {
    budget: Option<Budget>,
    used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Budget {
    limit: i64,
    remaining: i64,
}

impl CostState {
    pub(crate) fn set_budget(&mut self, limit: i64) {
        self.budget = Some(Budget {
            limit,
            remaining: limit,
        });
        self.used = 0;
    }

    pub(crate) fn used(&self) -> u64 {
        self.used
    }

    pub(crate) fn remaining(&self) -> i64 {
        self.budget.map_or(i64::MAX, |budget| budget.remaining)
    }

    pub(crate) fn limit(&self) -> i64 {
        self.budget.map_or(i64::MAX, |budget| budget.limit)
    }

    pub(crate) fn batch_needs_flush(&self, pending: u64) -> bool {
        self.budget
            .is_some_and(|budget| budget.remaining.saturating_sub_unsigned(pending) <= 0)
    }

    pub(crate) fn meter(&mut self) -> CostMeter<'_> {
        match &mut self.budget {
            Some(budget) => CostMeter::finite_budget(&mut budget.remaining, &mut self.used),
            None => CostMeter::count_only(&mut self.used),
        }
    }

    // Retain the existing snapshot layout, including canonical unbounded sentinels.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_fields(&self) -> (i64, i64, bool, u64) {
        (
            self.remaining(),
            self.limit(),
            self.budget.is_some(),
            self.used,
        )
    }

    /// Rebuild counters from snapshot fields, rejecting any combination the
    /// runtime cannot reach: an unconfigured meter must carry the unbounded
    /// sentinels, and a configured one must satisfy
    /// `remaining == limit - used` (saturating), with no use against a
    /// budget that started exhausted.
    #[cfg(feature = "snapshot")]
    pub(crate) fn from_snapshot(
        remaining: i64,
        limit: i64,
        configured: bool,
        used: u64,
    ) -> Option<Self> {
        let budget = if configured {
            if (limit <= 0 && used != 0) || remaining != limit.saturating_sub_unsigned(used) {
                return None;
            }
            Some(Budget { limit, remaining })
        } else {
            if remaining != i64::MAX || limit != i64::MAX {
                return None;
            }
            None
        };
        Some(Self { budget, used })
    }
}

/// Tracks costs either without a budget or against a finite budget.
///
/// The meter deliberately borrows only counters. This keeps data-dependent
/// native work independent from [`crate::State`], so non-VM components can use
/// it without depending on the VM module.
pub(crate) enum CostMeter<'a> {
    /// Counts work while no budget has been configured.
    CountOnly { used: &'a mut u64 },
    /// Counts work and checks it against a configured budget.
    FiniteBudget {
        remaining: &'a mut i64,
        used: &'a mut u64,
    },
}

impl<'a> CostMeter<'a> {
    /// Creates a meter that only records consumed cost.
    pub(crate) fn count_only(used: &'a mut u64) -> Self {
        Self::CountOnly { used }
    }

    /// Creates a meter for an explicitly configured finite budget.
    pub(crate) fn finite_budget(remaining: &'a mut i64, used: &'a mut u64) -> Self {
        Self::FiniteBudget { remaining, used }
    }

    /// Charges `n` unit costs with the exact outcome of `n` consecutive
    /// `consume(1)` calls: on a finite budget the charge stops at zero
    /// remaining and reports blocked, leaving `used`/`remaining` exactly
    /// where the unit-at-a-time loop would have left them. This lets scans
    /// that charge per byte batch their accounting without becoming
    /// observably different from the loop they replaced.
    #[inline]
    pub(crate) fn consume_units(&mut self, n: u64) -> bool {
        match self {
            Self::CountOnly { used } => {
                **used = (**used).saturating_add(n);
                true
            }
            Self::FiniteBudget { remaining, used } => {
                if n == 0 {
                    return true;
                }
                if **remaining <= 0 {
                    return false;
                }
                let allowed = n.min(**remaining as u64);
                **remaining = (**remaining).saturating_sub_unsigned(allowed);
                **used = (**used).saturating_add(allowed);
                allowed == n
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
            Self::CountOnly { used } => {
                **used = (**used).saturating_add(cost);
                true
            }
            Self::FiniteBudget { remaining, used } => {
                if cost > 0 && **remaining <= 0 {
                    return false;
                }
                **remaining = (**remaining).saturating_sub_unsigned(cost);
                **used = (**used).saturating_add(cost);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CostMeter;

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_meter_rejects_unreachable_counters() {
        use super::CostState;
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
            assert!(CostState::from_snapshot(fields.0, fields.1, fields.2, fields.3).is_none());
        }
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_meter_roundtrips_crossing_reset_and_saturation() {
        use super::CostState;
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
                cost.set_budget(limit);
            }
            for charge in [0, 3, 0, u64::MAX, 1] {
                let accepted = cost.meter().consume(charge);
                let fields = cost.snapshot_fields();
                let mut restored = CostState::from_snapshot(fields.0, fields.1, fields.2, fields.3)
                    .expect("runtime counters must be restorable");
                assert_eq!(restored.snapshot_fields(), fields);
                assert_eq!(restored.meter().consume(1), cost.meter().consume(1));
                assert_eq!(restored.snapshot_fields(), cost.snapshot_fields());
                if !accepted {
                    assert!(cost.remaining() <= 0);
                }
            }
            cost.set_budget(7);
            assert_eq!(cost.snapshot_fields(), (7, 7, true, 0));
        }
    }

    /// Charge `n` units one at a time - the loop `consume_units` must match.
    fn naive_units(remaining: &mut i64, used: &mut u64, n: u64) -> bool {
        for _ in 0..n {
            if !CostMeter::finite_budget(remaining, used).consume(1) {
                return false;
            }
        }
        true
    }

    #[test]
    fn consume_units_matches_the_unit_at_a_time_loop_exactly() {
        for start in [-3i64, 0, 1, 4, 5, 6, 100] {
            for n in [0u64, 1, 4, 5, 6, 50] {
                let (mut rem_a, mut used_a) = (start, 7u64);
                let (mut rem_b, mut used_b) = (start, 7u64);
                let batched = CostMeter::finite_budget(&mut rem_a, &mut used_a).consume_units(n);
                let looped = naive_units(&mut rem_b, &mut used_b, n);
                assert_eq!(batched, looped, "blocked mismatch at start={start} n={n}");
                assert_eq!(rem_a, rem_b, "remaining mismatch at start={start} n={n}");
                assert_eq!(used_a, used_b, "used mismatch at start={start} n={n}");
            }
        }
    }

    #[test]
    fn direct_and_native_meters_share_budget_semantics() {
        use crate::vm::State;
        for budget in [None, Some(-1), Some(0), Some(1), Some(10)] {
            let mut direct = State::new();
            let mut native = State::new();
            if let Some(budget) = budget {
                direct.set_cost_budget(budget);
                native.set_cost_budget(budget);
            }
            for cost in [0, 3, 0, 8, 1, u64::MAX, 0, 1] {
                let accepted = direct.consume_cost(cost).is_ok();
                assert_eq!(accepted, native.cost_meter().consume(cost));
                assert_eq!(direct.cost_used(), native.cost_used());
                assert_eq!(direct.cost_remaining(), native.cost_remaining());
            }
            if budget.is_none() {
                assert_eq!(direct.cost_remaining(), i64::MAX);
                assert_eq!(direct.cost_used(), u64::MAX);
            }
        }
    }

    fn assert_argument(error: &crate::error::Error, number: isize) {
        match error.kind() {
            crate::error::ErrorKind::ArgError(arg) => assert_eq!(arg.arg_number(), number),
            kind => panic!("expected an argument error, got {kind:?}"),
        }
    }

    #[test]
    fn gmatch_string_checks_preserve_relative_indexes_in_nested_frames() {
        let mut state = crate::vm::State::new();
        state.push_nil().unwrap();
        state.push_nil().unwrap();
        state.push_nil().unwrap();
        state.stack_bottom = 2;
        assert_argument(
            &state
                .cached_pattern_matcher(1)
                .err()
                .expect("non-string pattern fails"),
            1,
        );
        assert_argument(
            &state.with_gmatch_matcher(1, 1, |_, _, _| ()).unwrap_err(),
            1,
        );
        assert_argument(&state.to_bytes(1).unwrap_err(), 1);
        assert_argument(&state.to_bytes(-1).unwrap_err(), -1);
        assert_argument(&state.push_bytes_from_stack_range(1, 0..0).unwrap_err(), 1);
        state.push_string("subject").unwrap();
        assert_argument(
            &state.with_gmatch_matcher(2, 1, |_, _, _| ()).unwrap_err(),
            1,
        );
    }

    #[test]
    fn consume_units_counts_without_a_budget() {
        let mut used = 3u64;
        assert!(CostMeter::count_only(&mut used).consume_units(9));
        assert_eq!(used, 12);
    }
}
