use super::ArgCount;
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
                // Always mark call base - needed when last arg is vararg or function call
                // Adjustment: if we've already pushed the table/receiver (FieldAccess or TableIndex),
                // subtract 1 from the base since the function will replace what's already there
                let adjustment = match &base_expr {
                    PrefixExp::Place(PlaceExp::FieldAccess(_) | PlaceExp::TableIndex) => 1,
                    _ => 0,
                };
                let mark_idx = self.chunk.code.len();
                self.push(Instr::mark_call_base(adjustment));
                self.eval_prefix_exp(&base_expr);
                let (num_args, last_exp) = self.parse_call_args()?;
                // If last arg is vararg, adjust to pass all varargs
                let num_args = if let ExpDesc::Vararg = last_exp {
                    self.chunk.code.pop(); // Instr::pop() Vararg(1)
                    self.push(Instr::vararg(u8::MAX)); // Push all varargs
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                    // Last argument is a function call - adjust it to return all values
                    let inner_num_args = match self.chunk.code.pop() {
                        Some(instr) if instr.opcode() == Instr::OP_CALL => instr.a(),
                        i => {
                            unreachable!("PrefixExp::FunctionCall but last instruction was {:?}", i)
                        }
                    };
                    self.push(Instr::call(ArgCount::Fixed(inner_num_args), RetCount::All)); // Return all values
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else {
                    // No special handling needed, remove the Instr::mark_call_base()
                    self.chunk.code.remove(mark_idx);
                    self.checked_fixed_arg_count(num_args as usize, 0)?
                };
                let prefix = PrefixExp::FunctionCall(num_args);
                self.parse_prefix_extension(prefix)
            }
            TokenType::LParenLineStart => {
                let pos = self.input.next()?.start;
                Err(self.error_at(SyntaxError::LParenLineStart, pos))
            }
            TokenType::Colon => {
                // Method call: obj:method(args) becomes obj.method(obj, args)
                // Always mark call base - needed when last arg is vararg or function call
                // Same adjustment logic as function calls
                let adjustment = match &base_expr {
                    PrefixExp::Place(PlaceExp::FieldAccess(_) | PlaceExp::TableIndex) => 1,
                    _ => 0,
                };
                let mark_idx = self.chunk.code.len();
                self.push(Instr::mark_call_base(adjustment));
                self.eval_prefix_exp(&base_expr);
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
                    self.chunk.code.pop(); // Instr::pop() Vararg(1)
                    self.push(Instr::vararg(u8::MAX)); // Push all varargs
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else if let ExpDesc::Prefix(PrefixExp::FunctionCall(_)) = last_exp {
                    // Last argument is a function call - adjust it to return all values
                    let inner_num_args = match self.chunk.code.pop() {
                        Some(instr) if instr.opcode() == Instr::OP_CALL => instr.a(),
                        i => {
                            unreachable!("PrefixExp::FunctionCall but last instruction was {:?}", i)
                        }
                    };
                    self.push(Instr::call(ArgCount::Fixed(inner_num_args), RetCount::All)); // Return all values
                    u8::MAX // Signal to VM to calculate arg count from call base
                } else {
                    // No special handling needed, remove the Instr::mark_call_base()
                    self.chunk.code.remove(mark_idx);
                    self.checked_fixed_arg_count(num_args as usize, 1)?
                };
                // Stack: [method, obj, arg1, arg2, ...]
                let prefix = PrefixExp::FunctionCall(num_args);
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
                // Cut off the "0x"
                let text = &self.get_text(tok)[2..];
                let number = u128::from_str_radix(text, 16)
                    .map_err(|_| self.error_at(SyntaxError::BadNumber, tok.start))?
                    as f64;
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
