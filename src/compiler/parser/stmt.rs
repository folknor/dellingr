use super::super::ast::{Block, Expr, ExprKind, IfArm, Place, Stmt};
use super::Parser;
use super::Result;
use super::SyntaxError;
use super::TokenKind;
use super::expr::Prefix;
use crate::limits::{MAX_ASSIGNMENT_OFFSET, MAX_FIXED_RESULTS};

impl<'a> Parser<'a> {
    /// Parses a return statement. Return statements must always come last in a
    /// block.
    pub(super) fn parse_return(&mut self) -> Result<Stmt> {
        self.input.next()?; // 'return' keyword

        // As in reference Lua, only a block follower or `;` ends an empty
        // return; anything else must parse as an expression list, so a stray
        // token is an unexpected symbol rather than trailing input.
        let next = self.input.peek_type()?;
        let values = if !(Self::is_block_follow(next) || next == TokenKind::Semi) {
            self.parse_explist()?
        } else {
            Vec::new()
        };
        let line = self.input.last_line();
        self.input.try_pop(TokenKind::Semi)?;
        Ok(Stmt::Return { values, line })
    }

    /// Parses a statement which could be a variable assignment or a function call.
    ///
    /// As in reference Lua's `exprstat`, the token after the completed prefix
    /// expression decides: `=` or `,` makes it an assignment, which needs a
    /// variable; anything else makes it a call statement, which needs a call.
    /// Any other combination is a [`SyntaxError::MalformedStatement`] near
    /// that token.
    pub(super) fn parse_assign_or_call(&mut self) -> Result<Stmt> {
        let prefix = self.parse_prefix_exp()?;
        let assigning = matches!(
            self.input.peek_type()?,
            TokenKind::Assign | TokenKind::Comma
        );
        if prefix.is_call() {
            if assigning {
                return Err(self.malformed_statement());
            }
            return Ok(Stmt::Call(prefix.into_expr()));
        }
        if !assigning {
            return Err(self.malformed_statement());
        }
        match prefix.into_place() {
            Some(place) => self.parse_assign(place),
            None => Err(self.malformed_statement()),
        }
    }

    /// A [`SyntaxError::MalformedStatement`] near the next token.
    fn malformed_statement(&mut self) -> super::Error {
        match self.input.next() {
            Ok(tok) => self.err_near(tok, |found| SyntaxError::MalformedStatement { found }),
            Err(error) => error,
        }
    }

    /// Parses a variable assignment.
    fn parse_assign(&mut self, first: Place) -> Result<Stmt> {
        let mut targets = vec![first];
        while self.input.try_pop(TokenKind::Comma)?.is_some() {
            targets.push(self.parse_place_exp()?);
        }

        self.expect(TokenKind::Assign)?;
        let values = self.parse_explist()?;
        self.check_multi_assign(targets.len(), &values)?;

        // Field and index targets stay bounded by their position in the
        // target list, the limit the stack bytecode's store offsets set;
        // keeping it keeps the set of accepted programs unchanged.
        for (index, target) in targets.iter().enumerate().rev() {
            if matches!(target, Place::Field { .. } | Place::Index { .. }) {
                self.bounded_u8(
                    index,
                    MAX_ASSIGNMENT_OFFSET,
                    SyntaxError::AssignmentOffsetTooLarge,
                )?;
            }
        }

        Ok(Stmt::Assign {
            targets,
            values,
            line: self.input.last_line(),
        })
    }

    /// Parses an expression which can appear on the left side of an assignment.
    fn parse_place_exp(&mut self) -> Result<Place> {
        let prefix: Prefix = self.parse_prefix_exp()?;
        match prefix.into_place() {
            Some(place) => Ok(place),
            None => Err(self.malformed_statement()),
        }
    }

    /// Parses a `local` declaration.
    pub(super) fn parse_locals(&mut self) -> Result<Stmt> {
        self.input.next()?; // `local` keyword

        // Check for `local function name(...) ... end`
        if self.input.check_type(TokenKind::Function)? {
            return self.parse_local_function();
        }

        let first = self.local_slot_count()?;

        let names = self.parse_namelist()?;

        let values = if self.input.try_pop(TokenKind::Assign)?.is_some() {
            let values = self.parse_explist()?;
            self.check_multi_assign(names.len(), &values)?;
            values
        } else {
            Vec::new()
        };

        // Every new local needs a slot; the highest is checked first.
        for i in (0..names.len()).rev() {
            self.bounded_u8(i, crate::limits::MAX_LOCALS, SyntaxError::TooManyLocals)?
                .checked_add(first)
                .ok_or_else(|| self.error_here(SyntaxError::TooManyLocals))?;
        }

        // Bring the new variables into scope. It is important they are not
        // in scope until after this statement.
        let count = names.len();
        for name in names {
            self.add_local(name)?;
        }

        Ok(Stmt::Local {
            first,
            count,
            values,
        })
    }

    /// Parses `local function name(...) ... end`.
    /// This is equivalent to `local name; name = function(...) ... end`
    /// The name is in scope within the function body, allowing direct recursion.
    fn parse_local_function(&mut self) -> Result<Stmt> {
        self.input.next()?; // `function` keyword
        let name = self.expect_identifier()?;
        let reg = self.local_slot_count()?;

        // Add the local FIRST so it's in scope within the function body
        // (this allows recursive calls like `local function fib(n) ... fib(n-1) ... end`)
        self.add_local(name)?;

        let (nested, _) = self.parse_fndef(Some(name.to_string()), false)?;
        Ok(Stmt::LocalFunction { reg, nested })
    }

    /// Parse a comma-separated list of identifiers.
    fn parse_namelist(&mut self) -> Result<Vec<&'a str>> {
        let mut names = vec![self.expect_identifier()?];
        while self.input.try_pop(TokenKind::Comma)?.is_some() {
            names.push(self.expect_identifier()?);
        }
        Ok(names)
    }

    /// Parses a `for` loop, before we know whether it's generic (`for k, v in t do`) or
    /// numeric (`for i = 1,5 do`).
    pub(super) fn parse_for(&mut self) -> Result<Stmt> {
        self.input.next()?; // `for` keyword
        let first_name = self.expect_identifier()?;
        self.level_up();

        // Check what follows the first identifier to determine loop type
        let mut stmt = match self.input.peek_type()? {
            TokenKind::Assign => {
                // Numeric for: for i = start, stop [, step] do
                self.input.next()?; // consume '='
                self.parse_numeric_for(first_name)?
            }
            TokenKind::Comma | TokenKind::In => {
                // Generic for: for var1, var2, ... in explist do
                self.parse_generic_for(first_name)?
            }
            _ => {
                let tok = self.input.next()?;
                return Err(self.err_near(tok, |found| SyntaxError::ForSeparatorExpected { found }));
            }
        };
        let close = self.level_down()?;
        if let Stmt::NumericFor { scope_close, .. } | Stmt::GenericFor { scope_close, .. } =
            &mut stmt
        {
            *scope_close = close;
        }
        Ok(stmt)
    }

    /// Parses a numeric `for` loop, starting with the first expression after the `=`.
    fn parse_numeric_for(&mut self, name: &'a str) -> Result<Stmt> {
        // The start(current), stop and step are stored in three "hidden" local slots.
        let base = self.local_slot_count()?;

        // Control expressions are evaluated in the enclosing scope, before the
        // loop variable exists. Check capacity now so its error precedence is
        // unchanged without bringing the locals into scope too early.
        self.ensure_local_capacity(4)?;

        // First, all 3 control expressions are evaluated.
        let start = self.parse_expr()?;
        self.expect(TokenKind::Comma)?;
        let limit = self.parse_expr()?;

        // optional step value
        let step = self.parse_numeric_for_step()?;
        let prep_line = self.input.last_line();

        self.add_local("")?;
        self.add_local("")?;
        self.add_local("")?;

        // The actual local is in a fourth slot, so that it can be reassigned to.
        self.add_local(name)?;

        self.enter_loop(base);
        let (body, _) = self.parse_statements()?;
        self.expect(TokenKind::End)?;

        // Close the visible loop variable and body locals before its slot is
        // reused, giving a captured loop variable a fresh upvalue per iteration.
        // Omitted when nothing in the loop scope was captured.
        let body_close = self.loop_body_captured();
        let loop_line = self.input.last_line();
        let break_close = self.exit_loop()?;
        Ok(Stmt::NumericFor {
            base,
            start,
            limit,
            step,
            body,
            prep_line,
            loop_line,
            body_close,
            break_close,
            scope_close: None,
        })
    }

    /// Parses the optional step value of a numeric `for` loop.
    fn parse_numeric_for_step(&mut self) -> Result<Expr> {
        let next_token = self.input.next()?;
        match next_token.typ {
            TokenKind::Comma => {
                let step = self.parse_expr()?;
                self.expect(TokenKind::Do)?;
                Ok(step)
            }
            TokenKind::Do => {
                let i = self.find_or_add_number(1.0)?;
                Ok(Expr::new(ExprKind::Num(i)))
            }
            _ => Err(self.err_unexpected(next_token, TokenKind::Do)),
        }
    }

    /// Parses a generic `for` loop: `for var1, var2, ... in explist do body end`
    fn parse_generic_for(&mut self, first_name: &'a str) -> Result<Stmt> {
        // Collect all loop variable names
        let mut names = vec![first_name];
        while self.input.try_pop(TokenKind::Comma)?.is_some() {
            names.push(self.expect_identifier()?);
        }
        self.expect(TokenKind::In)?;

        let num_vars = self.bounded_u8(
            names.len(),
            usize::from(u8::MAX),
            SyntaxError::TooManyLocals,
        )?;
        let local_count = 3usize
            .checked_add(names.len())
            .ok_or_else(|| self.error_here(SyntaxError::TooManyLocals))?;

        // The hidden control variables: iterator function, state, control var
        let base = self.local_slot_count()?;

        // The expressions run in the enclosing scope. Preserve the existing
        // local-limit error without declaring the future loop locals early.
        self.ensure_local_capacity(local_count)?;

        // Evaluate the expression list (should produce iterator, state, initial)
        let exprs = self.parse_explist()?;
        self.check_multi_assign(3, &exprs)?;

        self.add_local("")?; // iterator function (slot 0)
        self.add_local("")?; // state (slot 1)
        self.add_local("")?; // control variable (slot 2)

        // Add the visible loop variables
        for name in &names {
            self.add_local(name)?;
        }

        self.expect(TokenKind::Do)?;
        let call_line = self.input.last_line();

        self.enter_loop(base);
        let (body, _) = self.parse_statements()?;
        self.expect(TokenKind::End)?;

        // Close the visible loop variables and body locals before their slots are reused.
        // Omitted when nothing in the loop scope was captured.
        let body_close = self.loop_body_captured();
        let break_close = self.exit_loop()?;
        Ok(Stmt::GenericFor {
            base,
            vars: num_vars,
            exprs,
            body,
            call_line,
            body_close,
            break_close,
            scope_close: None,
        })
    }

    /// Checks that an expression list adjusted to `num_targets` values can
    /// encode its expandable tail: a final call or vararg that has to supply
    /// more than one value has a fixed result width.
    fn check_multi_assign(&self, num_targets: usize, values: &[Expr]) -> Result<()> {
        let fixed_prefix = values.len().saturating_sub(1);
        let tail_needed = num_targets.saturating_sub(fixed_prefix);
        if tail_needed > 1 && values.last().is_some_and(Expr::is_multi) {
            self.bounded_u8(
                tail_needed,
                MAX_FIXED_RESULTS,
                SyntaxError::TooManyFixedResults,
            )?;
        }
        Ok(())
    }

    /// Parses a `do ... end` statement.
    pub(super) fn parse_do(&mut self) -> Result<Stmt> {
        self.input.next()?; // `do` keyword
        self.level_up();
        let (stmts, _) = self.parse_statements()?;
        self.expect(TokenKind::End)?;
        let close = self.level_down()?;
        Ok(Stmt::Do(Block { stmts, close }))
    }

    /// Parses a `repeat ... until` statement.
    pub(super) fn parse_repeat(&mut self) -> Result<Stmt> {
        self.input.next()?; // `repeat` keyword
        self.level_up();

        // Track locals before body
        let body_locals_start = self.local_slot_count()?;

        self.enter_loop(body_locals_start);
        let (body, _) = self.parse_statements()?;
        self.expect(TokenKind::Until)?;
        let cond = self.parse_expr()?;

        // Close upvalues for any locals declared inside the loop body
        // (before the conditional jump back). Decided after the `until`
        // expression, which may itself capture a body local.
        let body_close = (self.local_slot_count()? > body_locals_start
            && self.loop_body_captured())
        .then_some(body_locals_start);

        let break_close = self.exit_loop()?;
        let scope_close = self.level_down()?;
        Ok(Stmt::Repeat {
            body,
            cond,
            body_close,
            break_close,
            scope_close,
        })
    }

    /// Parses a `while ... do ... end` statement.
    pub(super) fn parse_while(&mut self) -> Result<Stmt> {
        self.input.next()?;
        self.level_up();
        let cond = self.parse_expr()?;
        self.expect(TokenKind::Do)?;

        // Track locals before body
        let body_locals_start = self.local_slot_count()?;

        self.enter_loop(body_locals_start);
        let (body, _) = self.parse_statements()?;
        self.expect(TokenKind::End)?;

        // Close upvalues for any captured locals declared inside the loop body
        let body_close = (self.local_slot_count()? > body_locals_start
            && self.loop_body_captured())
        .then_some(body_locals_start);

        let break_close = self.exit_loop()?;
        let scope_close = self.level_down()?;

        Ok(Stmt::While {
            cond,
            body,
            body_close,
            break_close,
            scope_close,
        })
    }

    /// Parses an if-then statement, including any attached `else` or `elseif` branches.
    ///
    /// The arms are parsed in a loop, like reference Lua's `ifstat`: an
    /// `elseif` chain is flat, so the whole statement charges one syntax
    /// level however many arms it has.
    pub(super) fn parse_if_arm(&mut self) -> Result<Stmt> {
        self.with_syntax_level(1, Self::parse_if_chain)
    }

    fn parse_if_chain(&mut self) -> Result<Stmt> {
        let mut arms = Vec::new();
        let else_block = loop {
            self.input.next()?; // `if` or `elseif` keyword
            let cond = self.parse_expr()?;
            self.expect(TokenKind::Then)?;
            self.level_up();
            let (stmts, _) = self.parse_statements()?;
            let close = self.level_down()?;
            arms.push(IfArm {
                cond,
                block: Block { stmts, close },
            });

            match self.input.peek_type()? {
                TokenKind::ElseIf => {}
                TokenKind::Else => break Some(self.parse_else()?),
                _ => {
                    self.expect(TokenKind::End)?;
                    break None;
                }
            }
        };
        Ok(Stmt::If { arms, else_block })
    }

    /// Parses an `else` block.
    fn parse_else(&mut self) -> Result<Block> {
        self.level_up();
        self.input.next()?; // `else` keyword
        let (stmts, _) = self.parse_statements()?;
        self.expect(TokenKind::End)?;
        let close = self.level_down()?;
        Ok(Block { stmts, close })
    }
}
