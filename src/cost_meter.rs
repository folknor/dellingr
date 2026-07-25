//! Runtime cost accounting independent of VM state.

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
