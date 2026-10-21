use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::PlaceExp;
use super::PrefixExp;
use super::Result;
use super::RetCount;
use super::SyntaxError;
use super::TokenType;
use crate::limits::{MAX_ASSIGNMENT_OFFSET, MAX_FIXED_RESULTS};

impl<'a> Parser<'a> {
    /// Parses a return statement. Return statements must always come last in a
    /// block.
    pub(super) fn parse_return(&mut self) -> Result<()> {
        self.input.next()?; // 'return' keyword

        // Check if there's an expression following return
        let n = if self.is_expr_start()? {
            let (n, last_exp) = self.parse_explist()?;
            match last_exp {
                ExpDesc::MultiTail(tail) => {
                    self.open_tail(tail, RetCount::All)?;
                    u8::MAX
                }
                _ => n,
            }
        } else {
            0
        };

        // At a return statement boundary the operand stack contains exactly
        // this expression list. 255 fixed expressions deliberately use All:
        // the bytecode has no fixed-255 encoding, while rejecting this source
        // would remove an existing accepted program. Tail calls/varargs also
        // select All explicitly via the same decoded sentinel above.
        self.push(Instr::ret(RetCount::from_u8(n)));
        self.input.try_pop(TokenType::Semi)?;
        Ok(())
    }

    /// Returns true if the next token could be the start of an expression.
    fn is_expr_start(&mut self) -> Result<bool> {
        let ok = matches!(
            self.input.peek_type()?,
            TokenType::Identifier
                | TokenType::LParen
                | TokenType::LCurly
                | TokenType::LiteralNumber
                | TokenType::LiteralString
                | TokenType::Function
                | TokenType::Nil
                | TokenType::False
                | TokenType::True
                | TokenType::Not
                | TokenType::Hash
                | TokenType::Minus
                | TokenType::DotDotDot
        );
        Ok(ok)
    }

    /// Parses a statement which could be a variable assignment or a function call.
    pub(super) fn parse_assign_or_call(&mut self) -> Result<()> {
        match self.parse_prefix_exp()? {
            PrefixExp::Parenthesized => {
                let tok = self.input.next()?;
                Err(self.err_unexpected(tok, TokenType::Assign))
            }
            PrefixExp::FunctionCall(call) => {
                let (num_args, line) = (call.num_args(), call.line());
                self.push_at_line(Instr::call(num_args, RetCount::Fixed(0)), line);
                Ok(())
            }
            PrefixExp::Place(first_place) => self.parse_assign(first_place),
        }
    }

    /// Parses a variable assignment.
    fn parse_assign(&mut self, first_exp: PlaceExp) -> Result<()> {
        let mut places = vec![first_exp];
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            places.push(self.parse_place_exp()?);
        }

        self.expect(TokenType::Assign)?;
        let (num_rvals, last_exp) = self.parse_explist()?;
        let num_lvals = places.len();
        self.adjust_multi_assign(num_lvals, usize::from(num_rvals), &last_exp)?;

        places.reverse();
        for (i, place_exp) in places.into_iter().enumerate() {
            let instr = match &place_exp {
                PlaceExp::FieldAccess(_) | PlaceExp::TableIndex => {
                    let stack_offset = num_lvals - i - 1;
                    let stack_offset = self.bounded_u8(
                        stack_offset,
                        MAX_ASSIGNMENT_OFFSET,
                        SyntaxError::AssignmentOffsetTooLarge,
                    )?;
                    place_exp.set_instr(stack_offset)
                }
                _ => place_exp.set_instr(0),
            };
            self.push(instr);
        }

        Ok(())
    }

    /// Parses an expression which can appear on the left side of an assignment.
    fn parse_place_exp(&mut self) -> Result<PlaceExp> {
        match self.parse_prefix_exp()? {
            PrefixExp::Parenthesized | PrefixExp::FunctionCall(_) => {
                let tok = self.input.next()?;
                Err(self.err_unexpected(tok, TokenType::Assign))
            }
            PrefixExp::Place(place) => Ok(place),
        }
    }

    /// Parses a `local` declaration.
    pub(super) fn parse_locals(&mut self) -> Result<()> {
        self.input.next()?; // `local` keyword

        // Check for `local function name(...) ... end`
        if self.input.check_type(TokenType::Function)? {
            return self.parse_local_function();
        }

        let old_local_count = self.local_slot_count()?;

        let names = self.parse_namelist()?;

        let num_names = names.len();
        if self.input.try_pop(TokenType::Assign)?.is_some() {
            // Also perform the assignment
            let (num_rvalues, last_exp) = self.parse_explist()?;
            self.adjust_multi_assign(num_names, usize::from(num_rvalues), &last_exp)?;
        } else {
            // They've only been declared, just set them all nil
            for _ in &names {
                self.push(Instr::push_nil());
            }
        }

        // Actually perform the assignment
        for i in (0..num_names).rev() {
            let slot = self
                .bounded_u8(i, crate::limits::MAX_LOCALS, SyntaxError::TooManyLocals)?
                .checked_add(old_local_count)
                .ok_or_else(|| self.error_here(SyntaxError::TooManyLocals))?;
            self.push(Instr::set_local(slot));
        }

        // Bring the new variables into scope. It is important they are not
        // in scope until after this statement.
        for name in names {
            self.add_local(name)?;
        }

        Ok(())
    }

    /// Parses `local function name(...) ... end`.
    /// This is equivalent to `local name; name = function(...) ... end`
    /// The name is in scope within the function body, allowing direct recursion.
    fn parse_local_function(&mut self) -> Result<()> {
        self.input.next()?; // `function` keyword
        let name = self.expect_identifier()?;
        let local_slot = self.local_slot_count()?;

        // Add the local FIRST so it's in scope within the function body
        // (this allows recursive calls like `local function fib(n) ... fib(n-1) ... end`)
        self.add_local(name)?;

        // Parse the function definition (pushes a Closure instruction)
        self.parse_fndef_named(Some(name.to_string()))?;

        // Assign the closure to the local
        self.push(Instr::set_local(local_slot));

        Ok(())
    }

    /// Parse a comma-separated list of identifiers.
    fn parse_namelist(&mut self) -> Result<Vec<&'a str>> {
        let mut names = vec![self.expect_identifier()?];
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            names.push(self.expect_identifier()?);
        }
        Ok(names)
    }

    /// Parses a `for` loop, before we know whether it's generic (`for k, v in t do`) or
    /// numeric (`for i = 1,5 do`).
    pub(super) fn parse_for(&mut self) -> Result<()> {
        self.input.next()?; // `for` keyword
        let first_name = self.expect_identifier()?;
        self.level_up();

        // Check what follows the first identifier to determine loop type
        match self.input.peek_type()? {
            TokenType::Assign => {
                // Numeric for: for i = start, stop [, step] do
                self.input.next()?; // consume '='
                self.parse_numeric_for(first_name)?;
            }
            TokenType::Comma | TokenType::In => {
                // Generic for: for var1, var2, ... in explist do
                self.parse_generic_for(first_name)?;
            }
            _ => {
                let tok = self.input.next()?;
                return Err(self.err_unexpected(tok, TokenType::Assign));
            }
        }
        self.level_down()?;
        Ok(())
    }

    /// Parses a numeric `for` loop, starting with the first expression after the `=`.
    fn parse_numeric_for(&mut self, name: &'a str) -> Result<()> {
        // The start(current), stop and step are stored in three "hidden" local slots.
        let current_local_slot = self.local_slot_count()?;

        // Control expressions are evaluated in the enclosing scope, before the
        // loop variable exists. Check capacity now so its error precedence is
        // unchanged without bringing the locals into scope too early.
        self.ensure_local_capacity(4)?;

        // First, all 3 control expressions are evaluated.
        self.parse_expr()?;
        self.expect(TokenType::Comma)?;
        self.parse_expr()?;

        // optional step value
        self.parse_numeric_for_step()?;

        self.add_local("")?;
        self.add_local("")?;
        self.add_local("")?;

        // The actual local is in a fourth slot, so that it can be reassigned to.
        self.add_local(name)?;

        // The ForPrep command pulls three values off the stack and places them
        // into locals to use in the loop.
        let loop_start_instr_index = self.funcs.chunk.code.len();
        self.push(Instr::for_prep(current_local_slot, -1));

        // body
        self.enter_loop(current_local_slot);
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close the visible loop variable and body locals before its slot is reused.
        self.push(Instr::close_upvalues(current_local_slot + 3));

        let loop_end = self.funcs.chunk.code.len();
        let body_length = self.checked_jump_offset(loop_start_instr_index, loop_end + 1)?;
        self.push(Instr::for_loop(current_local_slot, -body_length));

        // Correct the ForPrep instruction. Through `patch_jump` like every
        // other forward patch, so its landing site raises `fold_barrier`.
        self.patch_jump(loop_start_instr_index, loop_end + 1, |offset| {
            Instr::for_prep(current_local_slot, offset)
        })?;

        self.exit_loop()?;
        Ok(())
    }

    /// Parses the optional step value of a numeric `for` loop.
    fn parse_numeric_for_step(&mut self) -> Result<()> {
        let next_token = self.input.next()?;
        match next_token.typ {
            TokenType::Comma => {
                self.parse_expr()?;
                self.expect(TokenType::Do)?;
                Ok(())
            }
            TokenType::Do => {
                let i = self.find_or_add_number(1.0)?;
                self.push(Instr::push_num(i));
                Ok(())
            }
            _ => Err(self.err_unexpected(next_token, TokenType::Do)),
        }
    }

    /// Parses a generic `for` loop: `for var1, var2, ... in explist do body end`
    fn parse_generic_for(&mut self, first_name: &'a str) -> Result<()> {
        // Collect all loop variable names
        let mut names = vec![first_name];
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            names.push(self.expect_identifier()?);
        }
        self.expect(TokenType::In)?;

        let num_loop_vars = self.bounded_u8(
            names.len(),
            usize::from(u8::MAX),
            SyntaxError::TooManyLocals,
        )?;
        let local_count = 3usize
            .checked_add(names.len())
            .ok_or_else(|| self.error_here(SyntaxError::TooManyLocals))?;

        // The hidden control variables: iterator function, state, control var
        let base_slot = self.local_slot_count()?;

        // The expressions run in the enclosing scope. Preserve the existing
        // local-limit error without declaring the future loop locals early.
        self.ensure_local_capacity(local_count)?;

        // Evaluate the expression list (should produce iterator, state, initial)
        // We expect exactly 3 values
        let (num_exprs, last_exp) = self.parse_explist()?;

        self.adjust_multi_assign(3, usize::from(num_exprs), &last_exp)?;

        self.add_local("")?; // iterator function (slot 0)
        self.add_local("")?; // state (slot 1)
        self.add_local("")?; // control variable (slot 2)

        // Add the visible loop variables
        for name in &names {
            self.add_local(name)?;
        }

        self.expect(TokenType::Do)?;

        // TForPrep: pop 3 values into the hidden locals
        self.push(Instr::tfor_prep(base_slot));

        // Loop structure:
        // TForCall - call iterator, place results in loop var slots
        // TForLoop - check if first result is nil, jump out if so
        // body
        // Jump back to TForCall

        let loop_start = self.funcs.chunk.code.len();
        self.push(Instr::tfor_call(base_slot, num_loop_vars));
        let tforloop_index = self.funcs.chunk.code.len();
        self.push(Instr::tfor_loop(base_slot, 0)); // placeholder offset

        // body
        self.enter_loop(base_slot);
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close the visible loop variables and body locals before their slots are reused.
        self.push(Instr::close_upvalues(base_slot + 3));

        // Jump back to TForCall
        let body_end = self.funcs.chunk.code.len();
        self.push(Instr::jump(self.checked_jump_offset(body_end, loop_start)?));

        // Patch the TForLoop to jump past the body
        self.patch_jump(tforloop_index, self.funcs.chunk.code.len(), |offset| {
            Instr::tfor_loop(base_slot, offset)
        })?;

        self.exit_loop()?;
        Ok(())
    }

    /// Adjust an emitted expression list so exactly `num_targets` values are
    /// left on the stack. A tail call or vararg can supply (or discard) the
    /// required number of values; every other tail is padded or discarded.
    fn adjust_multi_assign(
        &mut self,
        num_targets: usize,
        num_exprs: usize,
        last_exp: &ExpDesc,
    ) -> Result<()> {
        if num_exprs == 0 {
            return Err(self.internal_error("multi-assignment requires a nonempty expression list"));
        }
        let fixed_prefix = num_exprs - 1;
        let tail_needed = num_targets.saturating_sub(fixed_prefix);
        if tail_needed > 1
            && let ExpDesc::MultiTail(tail) = last_exp
        {
            let tail_needed = self.checked_multi_assign_width(tail_needed)?;
            self.open_tail(*tail, RetCount::Fixed(tail_needed))?;
            return Ok(());
        }
        if num_targets > num_exprs {
            for _ in num_exprs..num_targets {
                self.push(Instr::push_nil());
            }
        } else {
            for _ in num_targets..num_exprs {
                self.push(Instr::pop());
            }
        }

        Ok(())
    }

    /// Converts a fixed multi-assignment result width without allowing the
    /// `255` dynamic/all-results sentinel into a fixed-width instruction.
    fn checked_multi_assign_width(&self, width: usize) -> Result<u8> {
        self.bounded_u8(width, MAX_FIXED_RESULTS, SyntaxError::TooManyFixedResults)
    }

    /// Parses a `do ... end` statement.
    pub(super) fn parse_do(&mut self) -> Result<()> {
        self.input.next()?; // `do` keyword
        self.level_up();
        self.parse_statements()?;
        self.expect(TokenType::End)?;
        self.level_down()?;
        Ok(())
    }

    /// Parses a `repeat ... until` statement.
    pub(super) fn parse_repeat(&mut self) -> Result<()> {
        self.input.next()?; // `repeat` keyword
        self.level_up();

        // Track locals before body
        let body_locals_start = self.local_slot_count()?;

        let body_start = self.funcs.chunk.code.len();
        self.enter_loop(body_locals_start);
        self.parse_statements()?;
        self.expect(TokenType::Until)?;
        self.parse_expr()?;

        // Close upvalues for any locals declared inside the loop body
        // (before the conditional jump back)
        if self.local_slot_count()? > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        let (branch_index, branch_constructor) = self.push_branch_false();
        self.patch_jump(branch_index, body_start, branch_constructor)?;
        self.exit_loop()?;
        self.level_down()?;
        Ok(())
    }

    /// Parses a `while ... do ... end` statement.
    pub(super) fn parse_while(&mut self) -> Result<()> {
        // Structure of while loop instructions:
        // - Condition instructions
        // - `BranchFalse` to evaluate condition and skip body
        // - Body instructions
        // - CloseUpvalues for body-local variables
        // - `Jump` back to condition start
        self.input.next()?;
        self.level_up();
        let condition_start = self.funcs.chunk.code.len();
        self.parse_expr()?;
        self.expect(TokenType::Do)?;

        let (test_position, test_constructor) = self.push_branch_false();

        // Track locals before body
        let body_locals_start = self.local_slot_count()?;

        self.enter_loop(body_locals_start);
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close upvalues for any locals declared inside the loop body
        if self.local_slot_count()? > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        let body_end = self.funcs.chunk.code.len();
        self.push(Instr::jump(
            self.checked_jump_offset(body_end, condition_start)?,
        ));

        self.patch_jump(test_position, self.funcs.chunk.code.len(), test_constructor)?;

        self.exit_loop()?;
        self.level_down()?;

        Ok(())
    }

    /// Parses an if-then statement, including any attached `else` or `elseif` branches.
    /// Parses an `if` or `elseif` block and any subsequent `elseif` or `else`
    /// blocks in the same chain.
    ///
    /// The arms are parsed in a loop, like reference Lua's `ifstat`: an
    /// `elseif` chain is flat, so the whole statement charges one syntax
    /// level however many arms it has.
    pub(super) fn parse_if_arm(&mut self) -> Result<()> {
        self.with_syntax_level(1, Self::parse_if_chain)
    }

    fn parse_if_chain(&mut self) -> Result<()> {
        // Each non-final arm that can fall through ends in a `Jump` to the end
        // of the statement.
        let mut end_jumps = Vec::new();
        loop {
            self.input.next()?; // `if` or `elseif` keyword
            self.parse_expr()?;
            self.expect(TokenType::Then)?;
            self.level_up();

            let (branch_instr_index, branch_constructor) = self.push_branch_false();

            // An arm whose own last statement is `return` cannot fall off its
            // end, so its jump to the end of the statement would be dead code;
            // skip it. This asks the parser, not the last emitted opcode: an
            // arm ending in a nested `if ... then return end` also ends in a
            // `Return` instruction, yet the nested `if`'s false branch lands
            // after it and does fall through.
            let arm_returns = self.parse_statements()?;
            self.level_down()?;

            let next = self.input.peek_type()?;
            if !arm_returns && matches!(next, TokenType::ElseIf | TokenType::Else) {
                end_jumps.push(self.funcs.chunk.code.len());
                self.push(Instr::jump(0));
            }
            // A false condition lands on the next arm (or past the statement).
            let next_arm_index = self.funcs.chunk.code.len();
            self.patch_jump(branch_instr_index, next_arm_index, branch_constructor)?;
            match next {
                TokenType::ElseIf => {}
                TokenType::Else => {
                    self.parse_else()?;
                    break;
                }
                _ => {
                    self.expect(TokenType::End)?;
                    break;
                }
            }
        }
        let end = self.funcs.chunk.code.len();
        for jump_instr_index in end_jumps {
            self.patch_jump(jump_instr_index, end, Instr::jump)?;
        }
        Ok(())
    }

    /// Parses an `else` block.
    fn parse_else(&mut self) -> Result<()> {
        self.level_up();
        self.input.next()?; // `else` keyword
        self.parse_statements()?;
        self.expect(TokenType::End)?;
        self.level_down()?;
        Ok(())
    }
}
