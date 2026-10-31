//! The cost registry: every charge the VM and the standard library make.
//!
//! Each [`Charge`] names one charge site family: an operator opcode, or a
//! costed library function. A charge site measures its work in units (one
//! per operation, per byte, per element or per matcher step, as
//! `docs/budget.md` describes) and charges `units * weight`, with the weight
//! read from a [`CostModel`]. [`CostModel::DEFAULT`] gives every charge a
//! weight of 1, which is the cost model `COST_MODEL_VERSION` names.
//!
//! The registry lists only work that is costed. Structural opcodes and the
//! free library functions have no entry: they charge nothing under any model.

/// Defines [`Charge`] with its display names, in discriminant order.
macro_rules! charges {
    ($($variant:ident => $name:literal,)+) => {
        /// One costed operation family. See the module documentation.
        #[repr(u8)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(crate) enum Charge {
            $($variant,)+
        }

        impl Charge {
            /// Number of charges.
            pub(crate) const COUNT: usize = [$(stringify!($variant),)+].len();

            /// Every charge, in discriminant order.
            #[cfg(test)]
            pub(crate) const ALL: &'static [Charge] = &[$(Charge::$variant,)+];

            /// The operator or library function the charge belongs to.
            #[cfg(test)]
            pub(crate) const fn name(self) -> &'static str {
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
    Add => "+",
    Sub => "-",
    Mul => "*",
    Div => "/",
    Mod => "%",
    Pow => "^",
    Negate => "unary -",
    NewTable => "table constructor",
    TableWrite => "table write",
    ArrayElement => "table constructor array element",
    // Base library.
    Rawset => "rawset",
    // Math library: one unit per call.
    MathAbs => "math.abs",
    MathAcos => "math.acos",
    MathAsin => "math.asin",
    MathAtan => "math.atan",
    MathAtan2 => "math.atan2",
    MathCeil => "math.ceil",
    MathCos => "math.cos",
    MathDeg => "math.deg",
    MathExp => "math.exp",
    MathFloor => "math.floor",
    MathFmod => "math.fmod",
    MathLog => "math.log",
    MathMax => "math.max",
    MathMin => "math.min",
    MathModf => "math.modf",
    MathRad => "math.rad",
    MathRandom => "math.random",
    MathSin => "math.sin",
    MathSqrt => "math.sqrt",
    MathTan => "math.tan",
    // String library: every unit the function charges, including the
    // matcher and substring-scan work it runs.
    StringFind => "string.find",
    StringFormat => "string.format",
    StringGmatch => "string.gmatch",
    StringGsub => "string.gsub",
    StringLower => "string.lower",
    StringMatch => "string.match",
    StringReverse => "string.reverse",
    StringSub => "string.sub",
    StringUpper => "string.upper",
    // Table library.
    TableConcat => "table.concat",
    TableInsert => "table.insert",
    TableMove => "table.move",
    TablePack => "table.pack",
    TableRemove => "table.remove",
    TableSort => "table.sort",
}

/// A weight for every [`Charge`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CostModel {
    weights: [u64; Charge::COUNT],
}

impl CostModel {
    /// The model `COST_MODEL_VERSION` names: every weight is 1.
    pub(crate) const DEFAULT: CostModel = CostModel {
        weights: [1; Charge::COUNT],
    };

    /// The weight of one unit of `charge`.
    #[inline(always)]
    pub(crate) const fn weight(&self, charge: Charge) -> u64 {
        self.weights[charge as usize]
    }

    /// The cost of `units` units of `charge`, saturating.
    #[inline(always)]
    pub(crate) const fn cost(&self, charge: Charge, units: u64) -> u64 {
        units.saturating_mul(self.weight(charge))
    }
}

#[cfg(test)]
mod tests {
    use super::{Charge, CostModel};

    #[test]
    fn charges_are_listed_in_discriminant_order() {
        for (index, charge) in Charge::ALL.iter().enumerate() {
            assert_eq!(*charge as usize, index, "{}", charge.name());
        }
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
            assert_eq!(CostModel::DEFAULT.weight(charge), 1, "{}", charge.name());
            assert_eq!(CostModel::DEFAULT.cost(charge, 7), 7, "{}", charge.name());
        }
    }
}
