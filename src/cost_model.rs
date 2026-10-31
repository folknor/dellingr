//! The cost registry: every charge the VM and the standard library make.
//!
//! Each [`Charge`] names one family of costed work: an operator opcode, or a
//! costed library function. A charge site measures its work in units (one
//! per operation, per byte, per element or per matcher step, as
//! `docs/budget.md` describes) and charges `units * weight`, with the weight
//! read from the State's [`CostModel`]. The default model gives every charge
//! a weight of 1.
//!
//! The registry lists only work that is costed. Structural opcodes and the
//! free library functions have no entry: they charge nothing under any model.
//!
//! A State's [`Costing`] is fixed when it is built (see
//! [`crate::StateBuilder::costing`]) and never changes afterwards.

/// Defines [`Charge`] with its display names and explicit discriminants.
macro_rules! charges {
    ($($variant:ident = $number:literal => $name:literal,)+) => {
        /// One family of costed work. See [`CostModel`].
        ///
        /// The discriminants are snapshot data: a saved model stores one
        /// weight per charge in discriminant order. Adding, removing or
        /// reattributing a charge changes `COST_MODEL_VERSION`.
        #[non_exhaustive]
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Charge {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant = $number,
            )+
        }

        impl Charge {
            /// Every charge, in discriminant order.
            pub const ALL: &'static [Charge] = &[$(Charge::$variant,)+];

            /// The operator or library function the charge belongs to.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Charge::$variant => $name,)+
                }
            }
        }
    };
}

charges! {
    // Opcodes: one unit per executed instruction, except ArrayElement, which
    // is one unit per element a table constructor stores in its array part.
    Add = 0 => "+",
    Sub = 1 => "-",
    Mul = 2 => "*",
    Div = 3 => "/",
    Mod = 4 => "%",
    Pow = 5 => "^",
    Negate = 6 => "unary -",
    NewTable = 7 => "table constructor",
    TableWrite = 8 => "table write",
    ArrayElement = 9 => "table constructor array element",
    // Base library.
    Rawset = 10 => "rawset",
    // Math library: one unit per call.
    MathAbs = 11 => "math.abs",
    MathAcos = 12 => "math.acos",
    MathAsin = 13 => "math.asin",
    MathAtan = 14 => "math.atan",
    MathAtan2 = 15 => "math.atan2",
    MathCeil = 16 => "math.ceil",
    MathCos = 17 => "math.cos",
    MathDeg = 18 => "math.deg",
    MathExp = 19 => "math.exp",
    MathFloor = 20 => "math.floor",
    MathFmod = 21 => "math.fmod",
    MathLog = 22 => "math.log",
    MathMax = 23 => "math.max",
    MathMin = 24 => "math.min",
    MathModf = 25 => "math.modf",
    MathRad = 26 => "math.rad",
    MathRandom = 27 => "math.random",
    MathSin = 28 => "math.sin",
    MathSqrt = 29 => "math.sqrt",
    MathTan = 30 => "math.tan",
    // String library: every unit the function charges, including the
    // matcher and substring-scan work it runs.
    StringFind = 31 => "string.find",
    StringFormat = 32 => "string.format",
    StringGmatch = 33 => "string.gmatch",
    StringGsub = 34 => "string.gsub",
    StringLower = 35 => "string.lower",
    StringMatch = 36 => "string.match",
    StringReverse = 37 => "string.reverse",
    StringSub = 38 => "string.sub",
    StringUpper = 39 => "string.upper",
    // Table library.
    TableConcat = 40 => "table.concat",
    TableInsert = 41 => "table.insert",
    TableMove = 42 => "table.move",
    TablePack = 43 => "table.pack",
    TableRemove = 44 => "table.remove",
    TableSort = 45 => "table.sort",
}

impl Charge {
    /// Number of charges.
    pub(crate) const COUNT: usize = Self::ALL.len();

    /// The charge's position in [`Charge::ALL`] and in a model's weights.
    #[inline(always)]
    const fn index(self) -> usize {
        self as usize
    }
}

// Weights are indexed by discriminant, so the discriminants must be exactly
// 0..COUNT in declaration order.
const _: () = {
    let mut index = 0;
    while index < Charge::COUNT {
        assert!(Charge::ALL[index] as usize == index);
        index += 1;
    }
};

/// A weight for every [`Charge`]: the price list a metered State charges by.
///
/// A charge site charges `units * weight`, saturating. A weight of 0 makes
/// that work free. Weights are integers; to make one operation relatively
/// cheaper, raise the others.
///
/// ```
/// use dellingr::{Charge, CostModel};
///
/// let model = CostModel::default()
///     .with(Charge::Pow, 10)
///     .with(Charge::StringFormat, 0);
/// assert_eq!(model.weight(Charge::Pow), 10);
/// assert_eq!(model.weight(Charge::Add), 1);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CostModel {
    weights: [u32; Charge::COUNT],
}

impl Default for CostModel {
    /// The model `COST_MODEL_VERSION` documents: every weight is 1.
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CostModel {
    /// Every weight 1.
    pub(crate) const DEFAULT: CostModel = CostModel {
        weights: [1; Charge::COUNT],
    };

    /// A model with every weight 0: every registered charge is free, while
    /// host charges through [`crate::State::consume_cost`] and a budget still
    /// apply. To turn accounting off entirely, use [`Costing::Off`].
    #[must_use]
    pub const fn zero() -> Self {
        CostModel {
            weights: [0; Charge::COUNT],
        }
    }

    /// The weight of one unit of `charge`.
    #[must_use]
    #[inline(always)]
    pub const fn weight(&self, charge: Charge) -> u32 {
        self.weights[charge.index()]
    }

    /// Sets the weight of one unit of `charge`.
    pub fn set(&mut self, charge: Charge, weight: u32) {
        self.weights[charge.index()] = weight;
    }

    /// This model with the weight of `charge` set to `weight`.
    #[must_use]
    pub fn with(mut self, charge: Charge, weight: u32) -> Self {
        self.set(charge, weight);
        self
    }

    /// Every charge with its weight, in discriminant order. Together with
    /// `COST_MODEL_VERSION`, this identifies the costs a source produces.
    pub fn iter(&self) -> impl Iterator<Item = (Charge, u32)> + '_ {
        Charge::ALL
            .iter()
            .map(|&charge| (charge, self.weight(charge)))
    }

    /// The model with these weights, in discriminant order.
    #[cfg(feature = "snapshot")]
    pub(crate) const fn from_weights(weights: [u32; Charge::COUNT]) -> Self {
        CostModel { weights }
    }

    /// The cost of `units` units of `charge`, saturating.
    #[inline(always)]
    pub(crate) const fn cost(&self, charge: Charge, units: u64) -> u64 {
        units.saturating_mul(self.weight(charge) as u64)
    }
}

/// How a State accounts for cost. Fixed when the State is built.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Costing {
    /// Charge by this model. `cost_used` counts every charge, and a budget
    /// set with [`crate::State::set_cost_budget`] limits them.
    Metered(CostModel),
    /// No accounting at all: every charge, including host charges through
    /// [`crate::State::consume_cost`], is admitted without being counted,
    /// `cost_used` stays 0, and [`crate::State::set_cost_budget`] is refused.
    Off,
}

impl Default for Costing {
    /// Metered by the default model.
    fn default() -> Self {
        Costing::Metered(CostModel::default())
    }
}

#[cfg(test)]
mod tests {
    use super::{Charge, CostModel};

    /// Snapshots store one weight per charge in discriminant order, so the
    /// mapping is snapshot data. This list is written by hand, independent of
    /// the declaration: a change to it is a snapshot and cost-model change
    /// (`COST_MODEL_VERSION`), never a regeneration.
    const PINNED_ORDER: &[(u16, &str)] = &[
        (0, "+"),
        (1, "-"),
        (2, "*"),
        (3, "/"),
        (4, "%"),
        (5, "^"),
        (6, "unary -"),
        (7, "table constructor"),
        (8, "table write"),
        (9, "table constructor array element"),
        (10, "rawset"),
        (11, "math.abs"),
        (12, "math.acos"),
        (13, "math.asin"),
        (14, "math.atan"),
        (15, "math.atan2"),
        (16, "math.ceil"),
        (17, "math.cos"),
        (18, "math.deg"),
        (19, "math.exp"),
        (20, "math.floor"),
        (21, "math.fmod"),
        (22, "math.log"),
        (23, "math.max"),
        (24, "math.min"),
        (25, "math.modf"),
        (26, "math.rad"),
        (27, "math.random"),
        (28, "math.sin"),
        (29, "math.sqrt"),
        (30, "math.tan"),
        (31, "string.find"),
        (32, "string.format"),
        (33, "string.gmatch"),
        (34, "string.gsub"),
        (35, "string.lower"),
        (36, "string.match"),
        (37, "string.reverse"),
        (38, "string.sub"),
        (39, "string.upper"),
        (40, "table.concat"),
        (41, "table.insert"),
        (42, "table.move"),
        (43, "table.pack"),
        (44, "table.remove"),
        (45, "table.sort"),
    ];

    #[test]
    fn charge_order_matches_the_pinned_snapshot_order() {
        let actual: Vec<(u16, &str)> = Charge::ALL
            .iter()
            .map(|&charge| (charge as u16, charge.name()))
            .collect();
        assert_eq!(actual, PINNED_ORDER);
    }

    #[test]
    fn charge_names_are_unique() {
        let mut names: Vec<&str> = Charge::ALL.iter().map(|charge| charge.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Charge::COUNT);
    }

    #[test]
    fn default_model_weighs_every_charge_one() {
        for &charge in Charge::ALL {
            assert_eq!(CostModel::default().weight(charge), 1, "{}", charge.name());
            assert_eq!(CostModel::default().cost(charge, 7), 7, "{}", charge.name());
            assert_eq!(CostModel::zero().cost(charge, 7), 0, "{}", charge.name());
        }
    }

    #[test]
    fn weights_saturate() {
        let model = CostModel::default().with(Charge::StringSub, u32::MAX);
        assert_eq!(model.cost(Charge::StringSub, u64::MAX), u64::MAX);
        assert_eq!(model.cost(Charge::StringUpper, 3), 3);
    }
}
