//! Runtime cost accounting independent of VM state.

/// Version of the runtime cost model used by persisted state.
///
/// Version 1 was the original implicit model. Version 2 charges native string
/// and pattern work in addition to bytecode instructions.
pub const COST_MODEL_VERSION: u16 = 2;

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
    fn consume_units_counts_without_a_budget() {
        let mut used = 3u64;
        assert!(CostMeter::count_only(&mut used).consume_units(9));
        assert_eq!(used, 12);
    }
}
