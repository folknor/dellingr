use super::ArgCount;
use super::CallSite;
use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::PlaceExp;
use super::PrefixExp;
use super::Result;
use super::RetCount;
use super::SyntaxError;
use super::TokenType;

impl Parser<'_> {
    /// Parses a comma-separated list of expressions. Trailing and leading
    /// commas are not allowed. Returns how many expressions were parsed and
    /// a descriptor of the last expression.
    #[hotpath::measure]
    pub(super) fn parse_explist(&mut self) -> Result<(u8, ExpDesc)> {
        // An explist has to have at least one expression.
        let mut last_exp_desc = self.parse_expr()?;
        let mut num_expressions = 1;
        while let Some(token) = self.input.try_pop(TokenType::Comma)? {
            if num_expressions == u8::MAX {
                return Err(self.error_at(SyntaxError::TooManyExpressions, token.start));
            }
            last_exp_desc = self.parse_expr()?;
            num_expressions += 1;
        }

        Ok((num_expressions, last_exp_desc))
    }

    /// Parses a single expression.
    #[hotpath::measure]
    pub(super) fn parse_expr(&mut self) -> Result<ExpDesc> {
        self.enter_syntax_level()?;
        let result = self.parse_expr_inner();
        self.exit_syntax_level();
        result
    }

    fn parse_expr_inner(&mut self) -> Result<ExpDesc> {
        self.parse_or()
    }

    /// Parses an `or` expression. Precedence 8.
    fn parse_or(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_and()?;

        while self.input.try_pop(TokenType::Or)?.is_some() {
            exp_desc = ExpDesc::Other;
            let branch_instr_index = self.chunk.code.len();
            self.push(Instr::branch_true_keep(0));
            // If we don't short-circuit, pop the left-hand expression
            self.push(Instr::pop());
            self.parse_and()?;
            self.patch_jump(
                branch_instr_index,
                self.chunk.code.len(),
                Instr::branch_true_keep,
            )?;
        }

        Ok(exp_desc)
    }

    /// Parses `and` expression. Precedence 7.
    fn parse_and(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_comparison()?;

        while self.input.try_pop(TokenType::And)?.is_some() {
            exp_desc = ExpDesc::Other;
            let branch_instr_index = self.chunk.code.len();
            self.push(Instr::branch_false_keep(0));
            // If we don't short-circuit, pop the left-hand expression
            self.push(Instr::pop());
            self.parse_comparison()?;
            self.patch_jump(
                branch_instr_index,
                self.chunk.code.len(),
                Instr::branch_false_keep,
            )?;
        }

        Ok(exp_desc)
    }

    /// Parses a comparison expression. Precedence 6.
    ///
    /// `==`, `~=`, `<`, `<=`, `>`, `>=`
    fn parse_comparison(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_concat()?;
        loop {
            let instr = match self.input.peek_type()? {
                TokenType::Less => Instr::less(),
                TokenType::LessEqual => Instr::less_equal(),
                TokenType::Greater => Instr::greater(),
                TokenType::GreaterEqual => Instr::greater_equal(),
                TokenType::Equal => Instr::equal(),
                TokenType::NotEqual => Instr::not_equal(),
                _ => break,
            };
            exp_desc = ExpDesc::Other;
            self.input.next()?;
            self.parse_concat()?;
            self.push(instr);
        }
        Ok(exp_desc)
    }

    /// Parses a string concatenation expression (`..`). Precedence 5.
    /// Flattens chained `..` into a single OP_CONCAT(n) so `a .. b .. c`
    /// emits one concat with three operands instead of two two-operand
    /// concats with an intermediate string allocation between them.
    fn parse_concat(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_addition()?;
        let mut count: u32 = 1;
        while self.input.try_pop(TokenType::DotDot)?.is_some() {
            exp_desc = ExpDesc::Other;
            self.parse_addition()?;
            count += 1;
        }
        if count > 1 {
            let n = u8::try_from(count).map_err(|_| self.error(SyntaxError::TooManyExpressions))?;
            self.push(Instr::concat(n));
        }
        Ok(exp_desc)
    }

    /// Parses an addition expression (`+`, `-`). Precedence 4.
    fn parse_addition(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_multiplication()?;
        loop {
            let instr = match self.input.peek_type()? {
                TokenType::Plus => Instr::add(),
                TokenType::Minus => Instr::subtract(),
                _ => break,
            };
            exp_desc = ExpDesc::Other;
            self.input.next()?;
            self.parse_multiplication()?;
            self.push(instr);
        }
        Ok(exp_desc)
    }

    /// Parses a multiplication expression (`*`, `/`, `%`). Precedence 3.
    fn parse_multiplication(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_unary()?;
        loop {
            let instr = match self.input.peek_type()? {
                TokenType::Star => Instr::multiply(),
                TokenType::Slash => Instr::divide(),
                TokenType::Mod => Instr::modulo(),
                _ => break,
            };
            exp_desc = ExpDesc::Other;
            self.input.next()?;
            self.parse_unary()?;
            self.push(instr);
        }
        Ok(exp_desc)
    }

    /// Parses a unary expression (`not`, `#`, `-`). Precedence 2.
    fn parse_unary(&mut self) -> Result<ExpDesc> {
        self.enter_syntax_level()?;
        let result = self.parse_unary_inner();
        self.exit_syntax_level();
        result
    }

    fn parse_unary_inner(&mut self) -> Result<ExpDesc> {
        let instr = match self.input.peek_type()? {
            TokenType::Not => Instr::not(),
            TokenType::Hash => Instr::length(),
            TokenType::Minus => Instr::negate(),
            _ => {
                return self.parse_pow();
            }
        };
        self.input.next()?;
        self.parse_unary()?;
        self.push(instr);

        Ok(ExpDesc::Other)
    }

    /// Parse an exponentiation expression (`^`). Right-associative, Precedence 1.
    fn parse_pow(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_primary()?;
        if self.input.try_pop(TokenType::Caret)?.is_some() {
            exp_desc = ExpDesc::Other;
            self.parse_unary()?;
            self.push(Instr::pow());
        }

        Ok(exp_desc)
    }

    /// Parses a 'primary' expression. See `parse_prefix_exp` and `parse_expr_base` for details.
    fn parse_primary(&mut self) -> Result<ExpDesc> {
        match self.input.peek_type()? {
            TokenType::Identifier | TokenType::LParen | TokenType::LParenLineStart => {
                let prefix = self.parse_prefix_exp()?;
                self.eval_prefix_exp(&prefix);
                Ok(prefix.into())
            }
            _ => self.parse_expr_base(),
        }
    }

    /// Parses a `prefix expression`. Prefix expressions are the expressions
    /// which can appear on the left side of a function call, table index, or
    /// field access.
    #[hotpath::measure]
    pub(super) fn parse_prefix_exp(&mut self) -> Result<PrefixExp> {
        let tok = self.input.next()?;
        let prefix = match tok.typ {
            TokenType::Identifier => {
                let text = self.get_text(tok);
                let place = self.parse_prefix_identifier(text)?;
                place.into()
            }
            TokenType::LParen | TokenType::LParenLineStart => {
                self.parse_expr()?;
                self.expect(TokenType::RParen)?;
                PrefixExp::Parenthesized
            }
            _ => {
                return Err(self.err_unexpected(tok, TokenType::Identifier));
            }
        };
        self.parse_prefix_extension(prefix)
    }

    /// Attempts to parse an extension to a prefix expression: a field access,
    /// table index, or function/method call.
    #[hotpath::measure]
    fn parse_prefix_extension(&mut self, base_expr: PrefixExp) -> Result<PrefixExp> {
        self.enter_syntax_level()?;
        let result = self.parse_prefix_extension_inner(base_expr);
        self.exit_syntax_level();
        result
    }

    fn parse_prefix_extension_inner(&mut self, base_expr: PrefixExp) -> Result<PrefixExp> {
        match self.input.peek_type()? {
            TokenType::Dot => {
                self.eval_prefix_exp(&base_expr);
                self.input.next()?;
                let field_name = self.expect_identifier()?;
                let name_idx = self.find_or_add_string(field_name)?;
                let prefix = PlaceExp::FieldAccess(name_idx).into();
                self.parse_prefix_extension(prefix)
            }
            TokenType::LSquare => {
                self.eval_prefix_exp(&base_expr);
                self.input.next()?;
                self.parse_expr()?;
                self.expect(TokenType::RSquare)?;
                let prefix = PlaceExp::TableIndex.into();
                self.parse_prefix_extension(prefix)
            }
            TokenType::LParen | TokenType::LiteralString | TokenType::LCurly => {
                let line = self.input.peek()?.line;
                // Evaluate the callee FIRST, then mark. The mark records where
                // this call's frame begins, and after the callee is evaluated
                // it is always exactly one value on top - whether it pushed
                // itself (a name), was already there (a parenthesized
                // expression or constructor), replaced its receiver (a field
                // access), collapsed a receiver and key (an index), or is the
                // single result of an inner call.
                //
                // Marking before evaluation instead required guessing how many
                // values the callee expression had already left on the stack,
                // which was wrong for every shape except a plain name and a
                // field access, and is not even a fixed number for an inner
                // call - the base then pointed at the receiver and the VM
                // called that instead of the callee.
                self.eval_prefix_exp(&base_expr);
                // Always mark call base - needed when last arg is vararg or a
                // function call, both of which make the argument count dynamic.
                let mark_idx = self.chunk.code.len();
                self.push(Instr::mark_call_base(1));
                let (num_args, last_exp) = self.parse_call_args()?;
                // If last arg is vararg, adjust to pass all varargs
                let num_args = if let ExpDesc::Vararg = last_exp {
                    self.replace_last_instr(Instr::vararg(u8::MAX)); // Push all varargs
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                    // Last argument is a function call - adjust it to return all values
                    let old = *self
                        .chunk
                        .code
                        .last()
                        .expect("function call expression must end with a call instruction");
                    let inner_num_args = match old.opcode() {
                        Instr::OP_CALL => old.a(),
                        _ => unreachable!(
                            "PrefixExp::FunctionCall but last instruction was {:?}",
                            old
                        ),
                    };
                    self.replace_last_instr(Instr::call(
                        ArgCount::Fixed(inner_num_args),
                        RetCount::All,
                    )); // Return all values
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else {
                    // Keep parse-time instruction indices stable. Finalization
                    // strips this free placeholder with one jump remap.
                    self.chunk.code[mark_idx] = Instr::nop();
                    self.checked_fixed_arg_count(num_args as usize, 0)?
                };
                let prefix = PrefixExp::FunctionCall(CallSite::new(num_args, line));
                self.parse_prefix_extension(prefix)
            }
            TokenType::LParenLineStart => {
                let pos = self.input.next()?.start;
                Err(self.error_at(SyntaxError::LParenLineStart, pos))
            }
            TokenType::Colon => {
                let line = self.input.peek()?.line;
                // Method call: obj:method(args) becomes obj.method(obj, args).
                //
                // Same ordering as a normal call: evaluate the receiver first,
                // then mark, so the base always sits one slot below the top
                // regardless of what shape the receiver expression had.
                // The dup/get_field/swap below leaves the resolved method in
                // that marked slot with the receiver above it, which is exactly
                // the [callee, args...] layout the call expects.
                self.eval_prefix_exp(&base_expr);
                // Always mark call base - needed when last arg is vararg or a
                // function call, both of which make the argument count dynamic.
                let mark_idx = self.chunk.code.len();
                self.push(Instr::mark_call_base(1));
                self.input.next()?; // consume ':'
                let method_name = self.expect_identifier()?;
                let name_idx = self.find_or_add_string(method_name)?;

                // Stack: [obj]
                // Duplicate obj (need it as both receiver and first arg)
                self.push(Instr::dup());
                // Stack: [obj, obj]
                // Get the method from the object
                self.push(Instr::get_field(name_idx));
                // Stack: [obj, method]
                // Swap so method is below obj (Call expects [func, args...])
                self.push(Instr::swap());
                // Stack: [method, obj]

                // Now parse the arguments
                let (num_args, last_exp) = self.parse_call_args()?;
                // If last arg is vararg, adjust to pass all varargs
                let num_args = if let ExpDesc::Vararg = last_exp {
                    self.replace_last_instr(Instr::vararg(u8::MAX)); // Push all varargs
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                    // Last argument is a function call - adjust it to return all values
                    let old = *self
                        .chunk
                        .code
                        .last()
                        .expect("function call expression must end with a call instruction");
                    let inner_num_args = match old.opcode() {
                        Instr::OP_CALL => old.a(),
                        _ => unreachable!(
                            "PrefixExp::FunctionCall but last instruction was {:?}",
                            old
                        ),
                    };
                    self.replace_last_instr(Instr::call(
                        ArgCount::Fixed(inner_num_args),
                        RetCount::All,
                    )); // Return all values
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else {
                    // Keep parse-time instruction indices stable. Finalization
                    // strips this free placeholder with one jump remap.
                    self.chunk.code[mark_idx] = Instr::nop();
                    self.checked_fixed_arg_count(num_args as usize, 1)?
                };
                // Stack: [method, obj, arg1, arg2, ...]
                let prefix = PrefixExp::FunctionCall(CallSite::new(num_args, line));
                self.parse_prefix_extension(prefix)
            }
            _ => Ok(base_expr),
        }
    }

    /// Parses a 'base' expression, after eliminating any operators. This can be:
    /// * A literal number
    /// * A literal string
    /// * A function definition
    /// * One of the keywords `nil`, `false` or `true
    /// * A table constructor
    #[hotpath::measure]
    fn parse_expr_base(&mut self) -> Result<ExpDesc> {
        let tok = self.input.next()?;
        match tok.typ {
            TokenType::LCurly => self.parse_table()?,
            TokenType::LiteralNumber => {
                let text = self.get_text(tok);
                let number: f64 = text
                    .parse()
                    .map_err(|_| self.error_at(SyntaxError::BadNumber, tok.start))?;
                let idx = self.find_or_add_number(number)?;
                self.push(Instr::push_num(idx));
            }
            TokenType::LiteralHexNumber => {
                // The shared numeral parser handles integer, fraction, and
                // binary-exponent hex forms (C30) and rounds oversized
                // mantissas to the nearest f64 like reference Lua, instead of
                // erroring past u128 range.
                let text = self.get_text(tok);
                let number = crate::numeral::parse_lua_numeral(text.as_bytes())
                    .ok_or_else(|| self.error_at(SyntaxError::BadNumber, tok.start))?;
                let idx = self.find_or_add_number(number)?;
                self.push(Instr::push_num(idx));
            }
            TokenType::LiteralString => {
                self.push_literal_string(tok)?;
            }
            TokenType::Function => self.parse_fndef()?,
            TokenType::Nil => self.push(Instr::push_nil()),
            TokenType::False => self.push(Instr::push_bool(false)),
            TokenType::True => self.push(Instr::push_bool(true)),
            TokenType::DotDotDot => {
                // Check if we're in a vararg function
                if !self.chunk.is_vararg {
                    return Err(self.error(SyntaxError::UnexpectedTok(
                        "cannot use '...' outside a vararg function".to_string(),
                    )));
                }
                // Default: push 1 value (will be adjusted if in tail position)
                self.push(Instr::vararg(1));
                return Ok(ExpDesc::Vararg);
            }
            _ => {
                return Err(self.err_unexpected(tok, TokenType::Nil));
            }
        }
        Ok(ExpDesc::Other)
    }
}
