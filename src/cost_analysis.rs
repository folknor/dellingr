//! Static cost analysis: what [`crate::Program::analyze_cost`] returns and
//! how it renders.
//!
//! The analysis walks each chunk's bytecode once, recording per scope how
//! many units of each [`Charge`] its opcodes make (from the opcode table's
//! charging declaration, the same one dispatch reads) and how many `OP_CALL`
//! instructions it holds. Those counts describe the program, not the model:
//! cost is derived from them afterwards by one rule, the saturating sum of
//! `units * weight` over the charges. The model that priced the analysis is
//! recorded in it and named in its report.

use crate::COST_MODEL_VERSION;
use crate::compiler::Bytecode;
use crate::cost_model::{Charge, CostModel};
use crate::instr::Instr;

/// Cost breakdown for a single scope (function or main chunk).
/// Instruction, unit and call counts cover this scope's chunk only; nested
/// function bodies have their own `ScopeCost` entries.
///
/// Read-only: the costs are derived from the counts under the model the
/// enclosing [`CostAnalysis`] records, so they cannot be edited apart.
#[derive(Debug, Clone)]
pub struct ScopeCost {
    name: String,
    own_cost: u64,
    total_cost: u64,
    units: [u64; Charge::COUNT],
    call_instructions: u64,
    instructions: u64,
    nested: Vec<ScopeCost>,
}

impl ScopeCost {
    /// Name of this scope: `main`, a function's name, or `anonymous #N`
    /// (anonymous functions are numbered among themselves).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Static cost of this scope's own opcodes, each counted once at its
    /// occurrence in the bytecode: the saturating sum over charges of
    /// [`ScopeCost::units`] times the model's weight.
    pub fn own_cost(&self) -> u64 {
        self.own_cost
    }

    /// [`ScopeCost::own_cost`] plus the total cost of every nested function
    /// body, each counted once regardless of whether that function is
    /// invoked. Saturating.
    pub fn total_cost(&self) -> u64 {
        self.total_cost
    }

    /// Units of `charge` this scope's opcodes make, each opcode counted once:
    /// one per arithmetic, negation, table-creation or table-write opcode,
    /// and the encoded element count of each closed `SET_LIST` (an open one
    /// counts zero). Library charges are always zero: the analyzer does not
    /// see native work. Independent of the cost model.
    pub fn units(&self, charge: Charge) -> u64 {
        self.units[charge as usize]
    }

    /// Number of `OP_CALL` instructions in this scope's chunk. Generic-`for`
    /// iterator calls and metamethod-driven calls are not call instructions
    /// and are not counted. A statistic: calls have no static charge.
    pub fn function_calls(&self) -> u64 {
        self.call_instructions
    }

    /// Instruction count in this scope's chunk, excluding nested functions.
    pub fn instructions(&self) -> u64 {
        self.instructions
    }

    /// Nested scopes (functions defined in this scope), in definition order.
    pub fn nested(&self) -> &[ScopeCost] {
        &self.nested
    }

    fn has_counts(&self) -> bool {
        self.call_instructions > 0 || self.units.iter().any(|&units| units > 0)
    }

    fn analyze_chunk(chunk: &Bytecode, name: String, model: &CostModel) -> Self {
        let mut scope = ScopeCost {
            name,
            own_cost: 0,
            total_cost: 0,
            units: [0; Charge::COUNT],
            call_instructions: 0,
            instructions: 0,
            nested: Vec::new(),
        };

        // The plain `+=` on the counters here, and in `accumulate`, cannot
        // overflow `u64`, in debug (panic) or release (wrap) alike. A
        // `Program` comes only from `Program::compile`, whose source is at
        // most `limits::MAX_SOURCE_BYTES` bytes. Code generation walks the
        // syntax tree, whose nodes each consume at least one source byte, and
        // every loop in it iterates a list of tree nodes, so a node emits a
        // small constant number of instructions: the instruction count,
        // across all nested chunks together, is a small multiple of the
        // source size. The unit counters add 1 per instruction or a
        // SET_LIST's element count (at most `limits::MAX_TABLE_BATCH_SIZE`
        // per instruction and at most the source's element count over all of
        // them). Every such sum is therefore a small multiple of the source
        // size, which holds as long as `MAX_SOURCE_BYTES` stays many orders
        // of magnitude below 2^64. Costs are those counts times model weights
        // of up to `u32::MAX`, so costs saturate instead.
        for inst in &chunk.code {
            scope.instructions += 1;
            if let Some((charge, units)) = inst.static_units() {
                scope.units[charge as usize] += units;
            }
            if inst.opcode() == Instr::OP_CALL {
                scope.call_instructions += 1;
            }
        }
        // Pricing the summed units equals summing per-instruction prices,
        // saturation included: every term is non-negative, so both are
        // min(exact sum, u64::MAX).
        scope.own_cost = price(&scope.units, model);

        // Anonymous functions are numbered among themselves, so a named
        // function between two anonymous ones does not skew the numbering.
        let mut anonymous_count = 0usize;
        for nested_chunk in &chunk.nested {
            let nested_name = match &nested_chunk.name {
                Some(name) => name.to_string(),
                None => {
                    anonymous_count += 1;
                    format!("anonymous #{anonymous_count}")
                }
            };
            scope
                .nested
                .push(Self::analyze_chunk(nested_chunk, nested_name, model));
        }

        scope.total_cost = scope.nested.iter().fold(scope.own_cost, |total, nested| {
            total.saturating_add(nested.total_cost)
        });

        scope
    }

    fn fmt_indent(&self, f: &mut std::fmt::Formatter<'_>, indent: usize) -> std::fmt::Result {
        let pad = "  ".repeat(indent);

        // Counts do not depend on the model's weights, so a zero-cost scope
        // can still have operations to show in its breakdown.
        if self.own_cost == 0 && self.nested.is_empty() && !self.has_counts() {
            // Keep the marker on this zero-cost short form: TFOR_CALL is not
            // an OP_CALL but can invoke a costed iterator, so zero here does
            // not mean the scope has no runtime cost. Other scope costs are
            // already under the static-cost report heading.
            writeln!(f, "{}{}: cost 0 (static)", pad, self.name)?;
            return Ok(());
        }

        if self.nested.is_empty() {
            writeln!(f, "{}{}: cost {}", pad, self.name, self.own_cost)?;
        } else {
            writeln!(
                f,
                "{}{}: cost {} (own) / {} (total)",
                pad, self.name, self.own_cost, self.total_cost
            )?;
        }

        let inner_pad = "  ".repeat(indent + 1);
        for &charge in Charge::ALL {
            let units = self.units(charge);
            if units > 0 {
                writeln!(f, "{}{}: {}", inner_pad, charge.name(), units)?;
            }
        }
        // Calls are statically free but their instruction count still belongs
        // in the per-scope breakdown.
        if self.call_instructions > 0 {
            writeln!(
                f,
                "{}call instructions: {}",
                inner_pad, self.call_instructions
            )?;
        }

        for nested in &self.nested {
            nested.fmt_indent(f, indent + 1)?;
        }

        Ok(())
    }

    fn accumulate(&self, totals: &mut ScopeTotals) {
        for (total, units) in totals.units.iter_mut().zip(&self.units) {
            *total += units;
        }
        totals.call_instructions += self.call_instructions;
        totals.instructions += self.instructions;
        totals.function_count += self.nested.len() as u64;
        for nested in &self.nested {
            nested.accumulate(totals);
        }
    }
}

/// The one pricing rule: the saturating sum of `units * weight`.
fn price(units: &[u64; Charge::COUNT], model: &CostModel) -> u64 {
    Charge::ALL.iter().fold(0, |total: u64, &charge| {
        total.saturating_add(model.cost(charge, units[charge as usize]))
    })
}

/// Static cost analysis of a Lua script, priced under the [`CostModel`] it
/// records.
///
/// This analyzes the bytecode without executing it. The actual runtime cost
/// depends on which code paths are taken and how many loop iterations occur.
/// Its `Display` report names `COST_MODEL_VERSION` and every weight of the
/// model, so reports priced under different models are told apart.
#[derive(Debug, Clone)]
pub struct CostAnalysis {
    root: ScopeCost,
    model: CostModel,
}

impl CostAnalysis {
    pub(crate) fn analyze(chunk: &Bytecode, model: &CostModel) -> Self {
        CostAnalysis {
            root: ScopeCost::analyze_chunk(chunk, "main".to_string(), model),
            model: model.clone(),
        }
    }

    /// Root scope (main chunk).
    pub fn root(&self) -> &ScopeCost {
        &self.root
    }

    /// The model this analysis was priced under.
    pub fn model(&self) -> &CostModel {
        &self.model
    }

    /// Return the standard explanation of the limits of this static estimate.
    ///
    /// The command-line interface prints this text after its analysis report.
    pub const fn caveat() -> &'static str {
        concat!(
            "Static cost analysis counts each costed opcode once across the main chunk and every nested function body, ",
            "whether or not that body runs. Loops and branches count once. ",
            "Call opcodes have no static charge, and the analyzer does not inspect their callees, ",
            "so it excludes all native library charges, flat and data-dependent. It is not a runtime-cost bound or a native-cost estimate."
        )
    }

    fn totals(&self) -> ScopeTotals {
        let mut totals = ScopeTotals {
            units: [0; Charge::COUNT],
            call_instructions: 0,
            instructions: 0,
            function_count: 0,
        };
        self.root.accumulate(&mut totals);
        totals
    }

    fn write_model(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let default = CostModel::default();
        write!(f, "Cost model: version {COST_MODEL_VERSION}, ")?;
        if self.model == default {
            return writeln!(f, "every weight 1");
        }
        write!(f, "weights 1 except")?;
        let mut separator = " ";
        for (charge, weight) in self.model.iter() {
            if weight != default.weight(charge) {
                write!(f, "{separator}{} = {weight}", charge.name())?;
                separator = ", ";
            }
        }
        writeln!(f)
    }
}

struct ScopeTotals {
    units: [u64; Charge::COUNT],
    call_instructions: u64,
    instructions: u64,
    function_count: u64,
}

impl std::fmt::Display for CostAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let totals = self.totals();

        writeln!(f, "=== Cost Analysis ===")?;
        writeln!(f)?;
        writeln!(f, "Static cost estimate: {}", self.root.total_cost)?;
        self.write_model(f)?;
        writeln!(f)?;
        writeln!(f, "--- Costed Operations ---")?;
        // Each row prices one charge's units by the same saturating rule as
        // the estimate, so a zero-weight charge still shows its units.
        for (charge, weight) in self.model.iter() {
            let units = totals.units[charge as usize];
            if units > 0 {
                writeln!(
                    f,
                    "  {}: {} x {} = {}",
                    charge.name(),
                    units,
                    weight,
                    self.model.cost(charge, units)
                )?;
            }
        }
        writeln!(f)?;
        writeln!(f, "--- Statistics ---")?;
        writeln!(f, "  Total instructions:   {}", totals.instructions)?;
        writeln!(f, "  Function definitions: {}", totals.function_count)?;
        writeln!(f, "  Call instructions:    {}", totals.call_instructions)?;
        writeln!(f)?;
        writeln!(f, "--- Per-Scope Breakdown ---")?;
        self.root.fmt_indent(f, 0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArgCount, Program, Result, RetCount, State};

    fn analyze_cost(source: &str) -> Result<CostAnalysis> {
        Program::compile(source, None).map(|program| program.analyze_cost())
    }

    #[test]
    fn dynamic_set_list_has_zero_minimum_cost_when_empty() {
        let analysis = analyze_cost("local function f(...) return {...} end\nf()")
            .expect("variadic table constructor should parse");
        let nested = &analysis.root().nested()[0];

        assert_eq!(nested.own_cost(), 1);
        assert_eq!(nested.units(Charge::ArrayElement), 0);
        assert_eq!(analysis.root().total_cost(), 1);

        let mut state = State::new();
        Program::compile("local function f(...) return {...} end\nf()", None)
            .and_then(|program| state.load(&program))
            .expect("variadic table constructor should load");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("no-argument variadic call should run");
        assert_eq!(state.cost_used(), 1);

        let fixed = analyze_cost("return {1, 2}").expect("fixed table constructor should parse");
        assert_eq!(fixed.root().own_cost(), 3);
        assert_eq!(fixed.root().units(Charge::ArrayElement), 2);
    }

    // Keep this as a tripwire against the old "(free)" label returning.
    #[test]
    fn zero_static_cost_generic_for_marks_the_estimate_as_static() {
        let analysis = analyze_cost("for value in iterator do end")
            .expect("generic-for iterator program should parse");
        assert_eq!(analysis.root().own_cost(), 0);
        assert_eq!(analysis.root().function_calls(), 0);

        let rendered = analysis.to_string();
        assert!(rendered.contains("main: cost 0 (static)"), "{rendered}");
        assert!(!rendered.contains("(free)"), "{rendered}");
    }

    fn report_fixture(model: CostModel) -> CostAnalysis {
        let mut units = [0; Charge::COUNT];
        units[Charge::Add as usize] = 1;
        units[Charge::Pow as usize] = 2;
        units[Charge::Negate as usize] = 1;
        units[Charge::NewTable as usize] = 1;
        units[Charge::TableWrite as usize] = 1;
        units[Charge::ArrayElement as usize] = 2;
        let own_cost = price(&units, &model);
        CostAnalysis {
            root: ScopeCost {
                name: "main".to_string(),
                own_cost,
                total_cost: own_cost,
                units,
                call_instructions: 1,
                instructions: 9,
                nested: Vec::new(),
            },
            model,
        }
    }

    #[test]
    fn cost_analysis_display_preserves_report_bytes() {
        assert_eq!(
            report_fixture(CostModel::default()).to_string(),
            format!(
                concat!(
                    "=== Cost Analysis ===\n",
                    "\n",
                    "Static cost estimate: 8\n",
                    "Cost model: version {}, every weight 1\n",
                    "\n",
                    "--- Costed Operations ---\n",
                    "  +: 1 x 1 = 1\n",
                    "  ^: 2 x 1 = 2\n",
                    "  unary -: 1 x 1 = 1\n",
                    "  table constructor: 1 x 1 = 1\n",
                    "  table write: 1 x 1 = 1\n",
                    "  table constructor array element: 2 x 1 = 2\n",
                    "\n",
                    "--- Statistics ---\n",
                    "  Total instructions:   9\n",
                    "  Function definitions: 0\n",
                    "  Call instructions:    1\n",
                    "\n",
                    "--- Per-Scope Breakdown ---\n",
                    "main: cost 8\n",
                    "  +: 1\n",
                    "  ^: 2\n",
                    "  unary -: 1\n",
                    "  table constructor: 1\n",
                    "  table write: 1\n",
                    "  table constructor array element: 2\n",
                    "  call instructions: 1\n",
                ),
                COST_MODEL_VERSION
            )
        );
    }

    #[test]
    fn report_names_every_non_default_weight_and_prices_each_charge() {
        let model = CostModel::default()
            .with(Charge::Pow, 10)
            .with(Charge::Add, 0)
            .with(Charge::StringFormat, 3);
        let rendered = report_fixture(model).to_string();
        let header = format!(
            "Static cost estimate: 25\n\
             Cost model: version {COST_MODEL_VERSION}, weights 1 except + = 0, ^ = 10, string.format = 3\n"
        );
        assert!(rendered.contains(&header), "{rendered}");
        assert!(rendered.contains("  +: 1 x 0 = 0\n"), "{rendered}");
        assert!(rendered.contains("  ^: 2 x 10 = 20\n"), "{rendered}");
    }

    #[test]
    fn static_analysis_excludes_flat_and_data_dependent_native_charges() {
        let source = "math.floor(1.5); table.pack(1); string.upper('abcd')";
        let analysis = analyze_cost(source).expect("native-call program should parse");
        assert_eq!(analysis.root().total_cost(), 0);
        for &charge in Charge::ALL {
            assert_eq!(analysis.root().units(charge), 0, "{}", charge.name());
        }

        let mut state = State::new();
        Program::compile(source, None)
            .and_then(|program| state.load(&program))
            .expect("native-call program should load");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("native-call program should run");
        assert_eq!(state.cost_used(), 6);
    }

    #[test]
    fn static_analysis_counts_costed_loop_body_once() {
        let closure_free = analyze_cost(
            "local x = 0; for i = 1, 3 do do local y = i; x = x + y end end; return x",
        )
        .expect("closure-free loop should parse");
        // Static analysis counts the loop body's single add once; the
        // runtime sweep below observes it three times.
        assert_eq!(closure_free.root().own_cost(), 1);
        assert_eq!(closure_free.root().total_cost(), 1);

        let dynamic = analyze_cost("local function f(...) return ... end; return f(1, f())")
            .expect("dynamic-call program should parse");
        assert_eq!(dynamic.root().own_cost(), 0);
        assert_eq!(dynamic.root().total_cost(), 0);

        let capture = analyze_cost(
            "local function make() local x = 1; return function() return x end end; return make()()",
        )
        .expect("closure-capture program should parse");
        assert_eq!(capture.root().own_cost(), 0);
        assert_eq!(capture.root().total_cost(), 0);

        for (source, returns, expected_cost) in [
            (
                "local x = 0; for i = 1, 3 do do local y = i; x = x + y end end; return x",
                RetCount::Fixed(1),
                3,
            ),
            (
                "local function f(...) return ... end; return f(1, f())",
                RetCount::All,
                0,
            ),
            (
                "local function make() local x = 1; return function() return x end end; return make()()",
                RetCount::Fixed(1),
                0,
            ),
        ] {
            let mut state = State::new();
            Program::compile(source, None)
                .and_then(|program| state.load(&program))
                .expect("cost fixture should load");
            state
                .call(ArgCount::Fixed(0), returns)
                .expect("cost fixture should run");
            assert_eq!(state.cost_used(), expected_cost, "{source}");
        }
    }
}
