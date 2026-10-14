//! Runtime cost accounting independent of VM state.

/// Version of the cost model: what `cost_used` a given source produces.
///
/// The version covers everything that decides the cost of identical source -
/// per-opcode charges, native library charges, and compiler emission (which
/// opcodes a source compiles to). Hosts may key stored cost fingerprints on
/// it; snapshots require an exact match. `tests/cost_golden.rs` pins
/// `cost_used` for a fixed corpus so a change cannot land without a bump.
///
/// Version 1 was the original implicit model. Version 2 charges native string
/// and pattern work in addition to bytecode instructions. Version 3 folds
/// literal arithmetic at compile time (folded operations no longer charge) and
/// charges `string.gmatch` pattern compilation once per iterator rather than
/// once per compilation-cache miss. Version 4 stops folding `^`, so literal
/// exponentiation charges like any other `^` (its result comes from the host
/// libm, which must not be baked into portable bytecode). Version 5 charges
/// opcode work still pending in the dispatch loop's batch when a runtime error
/// (other than budget exhaustion) ends a frame, so `cost_used` after a failed
/// callback counts every executed costed opcode, and charges the table library
/// per element: `table.insert`/`table.remove` one per element shifted,
/// `unpack`/`table.unpack` one per element visited and `table.pack` one per
/// stored argument, each with a minimum of 1.
pub const COST_MODEL_VERSION: u16 = 5;

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

    /// Selects the meter for a VM's persisted counters: a finite budget when
    /// one is configured, otherwise count-only. Every runtime borrowing path
    /// goes through here so the choice cannot drift between call sites.
    pub(crate) fn runtime(configured: bool, remaining: &'a mut i64, used: &'a mut u64) -> Self {
        if configured {
            Self::finite_budget(remaining, used)
        } else {
            Self::count_only(used)
        }
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

    fn assert_argument(error: crate::error::Error, number: isize) {
        match error.kind {
            crate::error::ErrorKind::ArgError(arg) => assert_eq!(arg.arg_number, number),
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
        assert_argument(state.memoize_gmatch_pattern(1).unwrap_err(), 1);
        assert_argument(
            state
                .gmatch_subject_matcher_and_cost_meter(1, 1)
                .err()
                .expect("non-string subject fails"),
            1,
        );
        assert_argument(state.to_bytes(1).unwrap_err(), 1);
        assert_argument(state.to_bytes(-1).unwrap_err(), -1);
        assert_argument(state.push_bytes_from_stack_range(1, 0..0).unwrap_err(), 1);
        state.push_string("subject").unwrap();
        assert_argument(
            state
                .gmatch_subject_matcher_and_cost_meter(2, 1)
                .err()
                .expect("non-string pattern fails"),
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
