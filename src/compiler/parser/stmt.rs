use std::cmp::Ordering;

use super::ArgCount;
use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::PlaceExp;
use super::PrefixExp;
use super::Result;
use super::RetCount;
use super::TokenType;

impl<'a> Parser<'a> {
    /// Parses a return statement. Return statements must always come last in a
    /// block.
    pub(super) fn parse_return(&mut self) -> Result<()> {
        self.input.next()?; // 'return' keyword

        // Check if there's an expression following return
        let n = if self.is_expr_start()? {
            let (n, last_exp) = self.parse_explist()?;
            // If the last expression is a function call or vararg, adjust to return all values
            match last_exp {
                ExpDesc::Prefix(PrefixExp::FunctionCall(num_args)) => {
                    self.chunk.code.pop(); // Instr::pop() the Call(num_args, 1) instruction
                    self.push(Instr::call(ArgCount::Fixed(num_args), RetCount::All)); // Emit Call with "return all"
                    u8::MAX
                }
                ExpDesc::Vararg => {
                    self.chunk.code.pop(); // Instr::pop() the Vararg(1) instruction
                    self.push(Instr::vararg(u8::MAX)); // Emit Vararg with "return all"
                    u8::MAX
                }
                _ => n,
            }
        } else {
            0
        };

        self.push(Instr::ret(RetCount::Fixed(n)));
        self.input.try_pop(TokenType::Semi)?;
        Ok(())
    }

    /// Returns true if the next token could be the start of an expression.
    fn is_expr_start(&mut self) -> Result<bool> {
        let ok = matches!(
            self.input.peek_type()?,
            TokenType::Identifier
                | TokenType::LParen
                | TokenType::LParenLineStart
                | TokenType::LCurly
                | TokenType::LiteralNumber
                | TokenType::LiteralHexNumber
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
            PrefixExp::FunctionCall(num_args) => {
                self.push(Instr::call(ArgCount::Fixed(num_args), RetCount::Fixed(0)));
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
        let num_lvals = places.len() as isize;
        let (num_rvals, last_exp) = self.parse_explist()?;
        let num_rvals = num_rvals as isize;
        let diff = num_lvals - num_rvals;
        if diff > 0 {
            if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                let num_args = match self.chunk.code.pop() {
                    Some(instr) if instr.opcode() == Instr::OP_CALL => instr.a(),
                    i => unreachable!("PrefixExp::FunctionCall but last instruction was {:?}", i),
                };
                self.push(Instr::call(
                    ArgCount::Fixed(num_args),
                    RetCount::Fixed(1 + diff as u8),
                ));
            } else {
                for _ in 0..diff {
                    self.push(Instr::push_nil());
                }
            }
        } else {
            // discard excess rvals
            for _ in diff..0 {
                self.push(Instr::pop());
            }
        }

        places.reverse();
        for (i, place_exp) in places.into_iter().enumerate() {
            let instr = match place_exp {
                PlaceExp::Local(i) => Instr::set_local(i),
                PlaceExp::Upvalue(i) => Instr::set_upvalue(i),
                PlaceExp::Global(i) => Instr::set_global(i),
                PlaceExp::Builtin(b) => Instr::set_builtin(b),
                PlaceExp::FieldAccess(literal_id) => {
                    let stack_offset = num_lvals as u8 - i as u8 - 1;
                    Instr::set_field(stack_offset, literal_id)
                }
                PlaceExp::TableIndex => {
                    let stack_offset = num_lvals as u8 - i as u8 - 1;
                    Instr::set_table(stack_offset)
                }
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

        let old_local_count = self.locals.len() as u8;

        let names = self.parse_namelist()?;

        let num_names = names.len() as u8;
        if self.input.try_pop(TokenType::Assign)?.is_some() {
            // Also perform the assignment
            let (num_rvalues, last_exp) = self.parse_explist()?;
            match num_names.cmp(&num_rvalues) {
                Ordering::Less => {
                    for _ in num_names..num_rvalues {
                        self.push(Instr::pop());
                    }
                }
                Ordering::Greater => {
                    if let ExpDesc::Prefix(PrefixExp::FunctionCall(num_args)) = last_exp {
                        self.chunk.code.pop(); // Instr::pop() the old 'Call' instruction
                        self.push(Instr::call(
                            ArgCount::Fixed(num_args),
                            RetCount::Fixed(1 + num_names - num_rvalues),
                        ));
                    } else if let ExpDesc::Vararg = last_exp {
                        self.chunk.code.pop(); // Instr::pop() the old 'Vararg(1)' instruction
                        self.push(Instr::vararg(1 + num_names - num_rvalues));
                    } else {
                        for _ in num_rvalues..num_names {
                            self.push(Instr::push_nil());
                        }
                    }
                }
                Ordering::Equal => (),
            }
        } else {
            // They've only been declared, just set them all nil
            for _ in &names {
                self.push(Instr::push_nil());
            }
        }

        // Actually perform the assignment
        for i in (0..num_names).rev() {
            self.push(Instr::set_local(i + old_local_count));
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
        let local_slot = self.locals.len() as u8;

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
        self.nest_level += 1;

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
        self.level_down();
        Ok(())
    }

    /// Parses a numeric `for` loop, starting with the first expression after the `=`.
    fn parse_numeric_for(&mut self, name: &str) -> Result<()> {
        // The start(current), stop and step are stored in three "hidden" local slots.
        let current_local_slot = self.locals.len() as u8;
        self.add_local("")?;
        self.add_local("")?;
        self.add_local("")?;

        // The actual local is in a fourth slot, so that it can be reassigned to.
        self.add_local(name)?;

        // Track locals count after loop variable (before body locals)
        let body_locals_start = self.locals.len() as u8;

        // First, all 3 control expressions are evaluated.
        self.parse_expr()?;
        self.expect(TokenType::Comma)?;
        self.parse_expr()?;

        // optional step value
        self.parse_numeric_for_step()?;

        // The ForPrep command pulls three values off the stack and places them
        // into locals to use in the loop.
        let loop_start_instr_index = self.chunk.code.len();
        self.push(Instr::for_prep(current_local_slot, -1));

        // body
        self.enter_loop();
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close upvalues for any locals declared inside the loop body.
        // This ensures each iteration captures its own values.
        if self.locals.len() as u8 > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        let body_length = (self.chunk.code.len() - loop_start_instr_index) as i16;
        self.push(Instr::for_loop(current_local_slot, -body_length));

        // Correct the ForPrep instruction.
        self.chunk.code[loop_start_instr_index] = Instr::for_prep(current_local_slot, body_length);

        self.exit_loop();
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
    fn parse_generic_for(&mut self, first_name: &str) -> Result<()> {
        // Collect all loop variable names
        let mut names = vec![first_name.to_string()];
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            names.push(self.expect_identifier()?.to_string());
        }
        self.expect(TokenType::In)?;

        // The hidden control variables: iterator function, state, control var
        let base_slot = self.locals.len() as u8;
        self.add_local("")?; // iterator function (slot 0)
        self.add_local("")?; // state (slot 1)
        self.add_local("")?; // control variable (slot 2)

        // Add the visible loop variables
        let num_loop_vars = names.len() as u8;
        for name in &names {
            self.add_local(name)?;
        }

        // Track locals count after loop variables (before body locals)
        let body_locals_start = self.locals.len() as u8;

        // Evaluate the expression list (should produce iterator, state, initial)
        // We expect exactly 3 values
        let (num_exprs, last_exp) = self.parse_explist()?;

        // Adjust to exactly 3 values on stack
        // If the last expression is a function call, we can adjust the Call
        // instruction to return the right number of values
        if num_exprs < 3 {
            if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                // Adjust the Call instruction to return enough values
                let num_args = match self.chunk.code.pop() {
                    Some(instr) if instr.opcode() == Instr::OP_CALL => instr.a(),
                    i => unreachable!("PrefixExp::FunctionCall but last instruction was {:?}", i),
                };
                self.push(Instr::call(
                    ArgCount::Fixed(num_args),
                    RetCount::Fixed(3 - num_exprs + 1),
                ));
            } else {
                // Push nils to make up the difference
                for _ in num_exprs..3 {
                    self.push(Instr::push_nil());
                }
            }
        } else if num_exprs > 3 {
            // Discard excess values
            for _ in 3..num_exprs {
                self.push(Instr::pop());
            }
        }

        self.expect(TokenType::Do)?;

        // TForPrep: pop 3 values into the hidden locals
        self.push(Instr::tfor_prep(base_slot));

        // Loop structure:
        // TForCall - call iterator, place results in loop var slots
        // TForLoop - check if first result is nil, jump out if so
        // body
        // Jump back to TForCall

        let loop_start = self.chunk.code.len();
        self.push(Instr::tfor_call(base_slot, num_loop_vars));
        let tforloop_index = self.chunk.code.len();
        self.push(Instr::tfor_loop(base_slot, 0)); // placeholder offset

        // body
        self.enter_loop();
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close upvalues for any locals declared inside the loop body.
        // This ensures each iteration captures its own values.
        if self.locals.len() as u8 > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        // Jump back to TForCall
        let body_end = self.chunk.code.len();
        self.push(Instr::jump(-((body_end + 1 - loop_start) as i16)));

        // Patch the TForLoop to jump past the body
        let exit_offset = (self.chunk.code.len() - tforloop_index - 1) as i16;
        self.chunk.code[tforloop_index] = Instr::tfor_loop(base_slot, exit_offset);

        self.exit_loop();
        Ok(())
    }

    /// Parses a `do ... end` statement.
    pub(super) fn parse_do(&mut self) -> Result<()> {
        self.input.next()?; // `do` keyword
        self.nest_level += 1;
        self.parse_statements()?;
        self.expect(TokenType::End)?;
        self.level_down();
        Ok(())
    }

    /// Parses a `repeat ... until` statement.
    pub(super) fn parse_repeat(&mut self) -> Result<()> {
        self.input.next()?; // `repeat` keyword
        self.nest_level += 1;

        // Track locals before body
        let body_locals_start = self.locals.len() as u8;

        let body_start = self.chunk.code.len() as i16;
        self.enter_loop();
        self.parse_statements()?;
        self.expect(TokenType::Until)?;
        self.parse_expr()?;

        // Close upvalues for any locals declared inside the loop body
        // (before the conditional jump back)
        if self.locals.len() as u8 > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        let expr_end = self.chunk.code.len() as i16;
        self.push(Instr::branch_false(body_start - (expr_end + 1)));
        self.exit_loop();
        self.level_down();
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
        self.nest_level += 1;
        let condition_start = self.chunk.code.len();
        self.parse_expr()?;
        self.expect(TokenType::Do)?;

        let test_position = self.chunk.code.len();
        self.push(Instr::branch_false(0));

        // Track locals before body
        let body_locals_start = self.locals.len() as u8;

        self.enter_loop();
        self.parse_statements()?;
        self.expect(TokenType::End)?;

        // Close upvalues for any locals declared inside the loop body
        if self.locals.len() as u8 > body_locals_start {
            self.push(Instr::close_upvalues(body_locals_start));
        }

        let body_end = self.chunk.code.len();
        self.push(Instr::jump(-((body_end + 1 - condition_start) as i16)));

        let body_len = body_end - test_position;
        self.chunk.code[test_position] = Instr::branch_false(body_len as i16);

        self.exit_loop();
        self.level_down();

        Ok(())
    }

    /// Parses an if-then statement, including any attached `else` or `elseif` branches.
    pub(super) fn parse_if(&mut self) -> Result<()> {
        self.parse_if_arm()
    }

    /// Parses an `if` or `elseif` block and any subsequent `elseif` or `else`
    /// blocks in the same chain.
    fn parse_if_arm(&mut self) -> Result<()> {
        self.input.next()?; // `if` or `elseif` keyword
        self.parse_expr()?;
        self.expect(TokenType::Then)?;
        self.nest_level += 1;

        let branch_instr_index = self.chunk.code.len();
        self.push(Instr::branch_false(0));

        self.parse_statements()?;
        let mut branch_target = self.chunk.code.len();

        self.close_if_arm()?;
        if self.chunk.code.len() > branch_target {
            // If the size has changed, the first instruction added was a
            // Jump, so we need to skip it.
            branch_target += 1;
        }

        let branch_offset = (branch_target - branch_instr_index - 1) as i16;
        self.chunk.code[branch_instr_index] = Instr::branch_false(branch_offset);
        Ok(())
    }

    /// Parses the closing keyword of an `if` or `elseif` arms, and any arms
    /// that may follow.
    fn close_if_arm(&mut self) -> Result<()> {
        self.level_down();
        match self.input.peek_type()? {
            TokenType::ElseIf => self.parse_else_or_elseif(true),
            TokenType::Else => self.parse_else_or_elseif(false),
            _ => {
                self.expect(TokenType::End)?;
                Ok(())
            }
        }
    }

    /// Parses an `elseif` or `else` block, and handles the `Jump` instruction
    /// for the end of the preceding block.
    fn parse_else_or_elseif(&mut self, elseif: bool) -> Result<()> {
        let jump_instr_index = self.chunk.code.len();
        self.push(Instr::jump(0));
        if elseif {
            self.parse_if_arm()?;
        } else {
            self.parse_else()?;
        }
        let new_len = self.chunk.code.len();
        let jump_len = new_len - jump_instr_index - 1;
        self.chunk.code[jump_instr_index] = Instr::jump(jump_len as i16);
        Ok(())
    }

    /// Parses an `else` block.
    fn parse_else(&mut self) -> Result<()> {
        self.nest_level += 1;
        self.input.next()?; // `else` keyword
        self.parse_statements()?;
        self.expect(TokenType::End)?;
        self.level_down();
        Ok(())
    }
}
