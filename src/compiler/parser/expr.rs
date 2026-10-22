use super::ArgCount;
use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::PlaceExp;
use super::PrefixExp;
use super::Result;
use super::RetCount;
use super::SyntaxError;
use super::TailKind;
use super::TokenType;
use crate::compiler::exp_desc::CallSite;
use crate::error::ParserFault;
use crate::limits::{MAX_EXPRESSIONS, SYNTAX_SUFFIX_COST};

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
            if num_expressions == MAX_EXPRESSIONS {
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
        self.with_syntax_level(1, Self::parse_expr_inner)
    }

    fn parse_expr_inner(&mut self) -> Result<ExpDesc> {
        self.parse_or()
    }

    /// Parses an `or` expression. Precedence 8.
    fn parse_or(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_and()?;

        while self.input.try_pop(TokenType::Or)?.is_some() {
            exp_desc = ExpDesc::Other;
            let branch_instr_index = self.funcs.chunk.code.len();
            self.push(Instr::branch_true_keep(0));
            // If we don't short-circuit, pop the left-hand expression
            self.push(Instr::pop());
            self.parse_and()?;
            self.patch_jump(
                branch_instr_index,
                self.funcs.chunk.code.len(),
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
            let branch_instr_index = self.funcs.chunk.code.len();
            self.push(Instr::branch_false_keep(0));
            // If we don't short-circuit, pop the left-hand expression
            self.push(Instr::pop());
            self.parse_comparison()?;
            self.patch_jump(
                branch_instr_index,
                self.funcs.chunk.code.len(),
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
            let line = self.input.next()?.line;
            self.parse_concat()?;
            self.push_at_line(instr, line);
        }
        Ok(exp_desc)
    }

    /// Parses a string concatenation expression (`..`). Precedence 5.
    /// Flattens chained `..` into a single OP_CONCAT(n) so `a .. b .. c`
    /// emits one concat with three operands instead of two two-operand
    /// concats with an intermediate string allocation between them.
    fn parse_concat(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_addition()?;
        let mut count: usize = 1;
        let mut line = None;
        while let Some(token) = self.input.try_pop(TokenType::DotDot)? {
            // A flattened chain reports its first operator, as Lua does when
            // it merges the right-associative concatenation instructions.
            line = line.or(Some(token.line));
            exp_desc = ExpDesc::Other;
            self.parse_addition()?;
            count += 1;
        }
        if count > 1 {
            let n = self.bounded_u8(
                count,
                usize::from(MAX_EXPRESSIONS),
                SyntaxError::TooManyExpressions,
            )?;
            let line =
                line.ok_or_else(|| self.internal_error(ParserFault::ConcatChainWithoutOperator))?;
            self.push_at_line(Instr::concat(n), line);
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
            let line = self.input.next()?.line;
            self.parse_multiplication()?;
            // The token match above selects only foldable arithmetic opcodes.
            self.push_arith(instr, line)?;
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
            let line = self.input.next()?.line;
            self.parse_unary()?;
            // The token match above selects only foldable arithmetic opcodes.
            self.push_arith(instr, line)?;
        }
        Ok(exp_desc)
    }

    /// Parses a unary expression (`not`, `#`, `-`). Precedence 2.
    fn parse_unary(&mut self) -> Result<ExpDesc> {
        self.with_syntax_level(1, Self::parse_unary_inner)
    }

    fn parse_unary_inner(&mut self) -> Result<ExpDesc> {
        let token_type = self.input.peek_type()?;
        let instr = match token_type {
            TokenType::Not => Instr::not(),
            TokenType::Hash => Instr::length(),
            TokenType::Minus => Instr::negate(),
            _ => {
                return self.parse_pow();
            }
        };
        let line = self.input.next()?.line;
        self.parse_unary()?;
        if token_type == TokenType::Minus {
            self.push_negate(line)?;
        } else {
            self.push_at_line(instr, line);
        }

        Ok(ExpDesc::Other)
    }

    /// Parse an exponentiation expression (`^`). Right-associative, Precedence 1.
    fn parse_pow(&mut self) -> Result<ExpDesc> {
        let mut exp_desc = self.parse_primary()?;
        if let Some(token) = self.input.try_pop(TokenType::Caret)? {
            exp_desc = ExpDesc::Other;
            self.parse_unary()?;
            // `^` is deliberately never constant-folded, unlike `+ - * / %`.
            // The runtime `pow` is the in-crate correctly rounded one
            // (`crmath::pow`), identical on every host, so folding would be
            // bit-safe; it stays unfolded because a folded literal is free
            // while a runtime `^` is charged as an instruction, and folding
            // would move cost charges and change compiled bytecode.
            self.push_at_line(Instr::pow(), token.line);
        }

        Ok(exp_desc)
    }

    /// Parses a 'primary' expression. See `parse_prefix_exp` and `parse_expr_base` for details.
    fn parse_primary(&mut self) -> Result<ExpDesc> {
        match self.input.peek_type()? {
            TokenType::Identifier | TokenType::LParen => {
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
                let text = self.get_text(tok)?;
                let place = self.parse_prefix_identifier(text)?;
                place.into()
            }
            TokenType::LParen => {
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

    /// Parses every extension of a prefix expression: field accesses, table
    /// indexes, and function/method calls, in a loop like reference Lua's
    /// `suffixedexp`. A suffix chain is flat, so its length adds no syntax
    /// depth. Parsing the contents of an index or call charges a level while
    /// those contents are active, in addition to their expression levels.
    #[hotpath::measure]
    fn parse_prefix_extension(&mut self, base_expr: PrefixExp) -> Result<PrefixExp> {
        let mut expr = base_expr;
        loop {
            // These suffixes retain this loop and the suffix parser while
            // descending into expressions. Keep their previous depth charge:
            // without it, nested calls and indexes allow more native frames
            // per syntax-budget tick than parenthesized expressions do. The
            // charge size and its reason live on SYNTAX_SUFFIX_COST.
            let nested = matches!(
                self.input.peek_type()?,
                TokenType::LSquare | TokenType::LParen | TokenType::LCurly | TokenType::Colon
            );
            let result = if nested {
                self.with_syntax_level(SYNTAX_SUFFIX_COST, |parser| {
                    parser.parse_prefix_suffix(&expr)
                })
            } else {
                self.parse_prefix_suffix(&expr)
            };
            match result? {
                Some(extended) => expr = extended,
                None => break,
            }
        }
        Ok(expr)
    }

    /// Parses one suffix applied to `base_expr`, or returns `None` (consuming
    /// nothing) when the next token does not start a suffix.
    fn parse_prefix_suffix(&mut self, base_expr: &PrefixExp) -> Result<Option<PrefixExp>> {
        let next = match self.input.peek_type()? {
            TokenType::Dot => {
                self.eval_prefix_exp(base_expr);
                self.input.next()?;
                let field_name = self.expect_identifier()?;
                let name_idx = self.find_or_add_string(field_name)?;
                PlaceExp::FieldAccess(name_idx).into()
            }
            TokenType::LSquare => {
                self.eval_prefix_exp(base_expr);
                self.input.next()?;
                self.parse_expr()?;
                self.expect(TokenType::RSquare)?;
                PlaceExp::TableIndex.into()
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
                self.eval_prefix_exp(base_expr);
                let mark_idx = self.mark_call_base();
                self.finish_call(mark_idx, 0, line)?
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
                self.eval_prefix_exp(base_expr);
                let mark_idx = self.mark_call_base();
                self.input.next()?; // consume ':'
                let method_name = self.expect_identifier()?;
                let name_idx = self.find_or_add_string(method_name)?;

                // Stack: [obj]
                // Duplicate obj (need it as both receiver and first arg)
                self.push(Instr::dup());
                // Stack: [obj, obj]
                // Get the method from the object
                self.push(Instr::get_field(name_idx, None));
                // Stack: [obj, method]
                // Swap so method is below obj (Call expects [func, args...])
                self.push(Instr::swap());
                // Stack: [method, obj]

                let call = self.finish_call(mark_idx, 1, line)?;
                // Stack: [method, obj, arg1, arg2, ...]
                call
            }
            _ => return Ok(None),
        };
        Ok(Some(next))
    }

    /// Emits the marker shared by normal and method calls after evaluating
    /// their callee or receiver.
    fn mark_call_base(&mut self) -> usize {
        let mark_idx = self.funcs.chunk.code.len();
        self.push(Instr::mark_call_base(1));
        mark_idx
    }

    /// Parses arguments and patches the call marker for a fixed or expandable
    /// final argument list.
    fn finish_call(
        &mut self,
        mark_idx: usize,
        implicit_args: usize,
        line: u32,
    ) -> Result<PrefixExp> {
        let (num_args, last_exp) = self.parse_call_args()?;
        let num_args = match last_exp {
            ExpDesc::MultiTail(tail) => {
                self.open_tail(tail, RetCount::All)?;
                ArgCount::Dynamic
            }
            _ => {
                // Keep parse-time instruction indices stable. Finalization
                // strips this free placeholder with one jump remap.
                self.funcs.chunk.code[mark_idx] = Instr::nop();
                ArgCount::Fixed(self.checked_fixed_arg_count(num_args as usize, implicit_args)?)
            }
        };
        Ok(PrefixExp::FunctionCall(CallSite::new(num_args, line)))
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
                // The lexer determines token boundaries; this shared parser
                // owns conversion and validates the complete numeral.
                let text = self.input.source_bytes(tok.range());
                let number = crate::numeral::parse_lua_numeral(text)
                    .ok_or_else(|| self.error_at(SyntaxError::BadNumber, tok.start))?;
                let idx = self.find_or_add_number(number)?;
                self.push(Instr::push_num(idx));
            }
            TokenType::LiteralString => {
                self.push_literal_string(tok)?;
            }
            TokenType::Function => self.parse_fndef(None, false)?,
            TokenType::Nil => self.push(Instr::push_nil()),
            TokenType::False => self.push(Instr::push_bool(false)),
            TokenType::True => self.push(Instr::push_bool(true)),
            TokenType::DotDotDot => {
                // Check if we're in a vararg function
                if !self.funcs.chunk.is_vararg {
                    return Err(self.error_here(SyntaxError::VarargOutsideVarargFunction));
                }
                // Default: push 1 value (will be adjusted if in tail position)
                self.push(Instr::vararg(RetCount::Fixed(1)));
                return Ok(ExpDesc::MultiTail(TailKind::Vararg));
            }
            _ => {
                return Err(self.err_unexpected(tok, TokenType::Nil));
            }
        }
        Ok(ExpDesc::Other)
    }
}
