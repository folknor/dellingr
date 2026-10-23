use super::super::ast::{
    ArithChain, ArithLink, CallSuffix, CompareChain, CompareLink, Expr, ExprKind, MethodSuffix,
    Place, Suffix, Suffixed,
};
use super::Parser;
use super::Result;
use super::SyntaxError;
use super::TokenKind;
use super::Variable;
use crate::instr::{ArithOp, CompareOp};
use crate::limits::{MAX_EXPRESSIONS, SYNTAX_SUFFIX_COST};

/// A parsed prefix expression: a variable or parenthesized expression and
/// the field, index, call and method suffixes applied to it.
pub(super) struct Prefix {
    base: PrefixBase,
    suffixes: Vec<Suffix>,
}

enum PrefixBase {
    /// A name, resolved and stamped where it was read.
    Var(Variable, u32),
    /// A parenthesized expression.
    Paren(Expr),
}

impl Prefix {
    /// Whether the prefix ends in a call or method call.
    pub(super) fn is_call(&self) -> bool {
        matches!(
            self.suffixes.last(),
            Some(Suffix::Call(_) | Suffix::Method(_))
        )
    }

    /// The prefix's value.
    pub(super) fn into_expr(self) -> Expr {
        let base = match self.base {
            PrefixBase::Var(variable, line) => variable_read(variable, line),
            PrefixBase::Paren(expr) => expr,
        };
        if self.suffixes.is_empty() {
            base
        } else {
            Expr::new(ExprKind::Suffixed(Box::new(Suffixed {
                base,
                suffixes: self.suffixes,
            })))
        }
    }

    /// The prefix as an assignment target, or `None` when it names no
    /// variable or field (a call or a parenthesized expression).
    pub(super) fn into_place(mut self) -> Option<Place> {
        match self.suffixes.pop() {
            None => match self.base {
                PrefixBase::Var(variable, _) => Some(match variable {
                    Variable::Local { reg, .. } => Place::Local(reg),
                    Variable::Upval(index) => Place::Upval(index),
                    Variable::Global(name) => Place::Global(name),
                    Variable::Builtin(builtin) => Place::Builtin(builtin),
                }),
                PrefixBase::Paren(_) => None,
            },
            Some(Suffix::Field { name, .. }) => Some(Place::Field {
                object: self.into_expr(),
                name,
            }),
            Some(Suffix::Index { key, .. }) => Some(Place::Index {
                object: self.into_expr(),
                key,
            }),
            Some(Suffix::Call(_) | Suffix::Method(_)) => None,
        }
    }
}

/// The read of a resolved variable stamped with `line`.
pub(super) fn variable_read(variable: Variable, line: u32) -> Expr {
    Expr::new(match variable {
        Variable::Local { reg, id } => ExprKind::Local { reg, id },
        Variable::Upval(index) => ExprKind::Upval(index),
        Variable::Global(name) => ExprKind::Global { name, line },
        Variable::Builtin(builtin) => ExprKind::Builtin(builtin),
    })
}

impl Parser<'_> {
    /// Parses a comma-separated list of expressions. Trailing and leading
    /// commas are not allowed.
    #[hotpath::measure]
    pub(super) fn parse_explist(&mut self) -> Result<Vec<Expr>> {
        // An explist has to have at least one expression.
        let mut exprs = vec![self.parse_expr()?];
        while let Some(token) = self.input.try_pop(TokenKind::Comma)? {
            if exprs.len() == usize::from(MAX_EXPRESSIONS) {
                return Err(self.error_at(SyntaxError::TooManyExpressions, token.start));
            }
            exprs.push(self.parse_expr()?);
        }
        Ok(exprs)
    }

    /// Parses a single expression.
    #[hotpath::measure]
    pub(super) fn parse_expr(&mut self) -> Result<Expr> {
        self.with_syntax_level(1, |parser| {
            let line = parser.input.peek()?.line;
            let mut expr = parser.parse_expr_inner()?;
            expr.line = line;
            Ok(expr)
        })
    }

    fn parse_expr_inner(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    /// Parses an `or` expression. Precedence 8.
    fn parse_or(&mut self) -> Result<Expr> {
        let first = self.parse_and()?;
        if !self.input.check_type(TokenKind::Or)? {
            return Ok(first);
        }
        let mut operands = vec![first];
        while self.input.try_pop(TokenKind::Or)?.is_some() {
            operands.push(self.parse_and()?);
        }
        Ok(Expr::new(ExprKind::Or(operands)))
    }

    /// Parses `and` expression. Precedence 7.
    fn parse_and(&mut self) -> Result<Expr> {
        let first = self.parse_comparison()?;
        if !self.input.check_type(TokenKind::And)? {
            return Ok(first);
        }
        let mut operands = vec![first];
        while self.input.try_pop(TokenKind::And)?.is_some() {
            operands.push(self.parse_comparison()?);
        }
        Ok(Expr::new(ExprKind::And(operands)))
    }

    /// Parses a comparison expression. Precedence 6.
    ///
    /// `==`, `~=`, `<`, `<=`, `>`, `>=`
    fn parse_comparison(&mut self) -> Result<Expr> {
        let first = self.parse_concat()?;
        let mut rest = Vec::new();
        loop {
            let op = match self.input.peek_type()? {
                TokenKind::Less => CompareOp::Lt,
                TokenKind::LessEqual => CompareOp::Le,
                TokenKind::Greater => CompareOp::Gt,
                TokenKind::GreaterEqual => CompareOp::Ge,
                TokenKind::Equal => CompareOp::Eq,
                TokenKind::NotEqual => CompareOp::Ne,
                _ => break,
            };
            let line = self.input.next()?.line;
            let rhs = self.parse_concat()?;
            rest.push(CompareLink { op, rhs, line });
        }
        if rest.is_empty() {
            Ok(first)
        } else {
            Ok(Expr::new(ExprKind::Compare(Box::new(CompareChain {
                first,
                rest,
            }))))
        }
    }

    /// Parses a string concatenation expression (`..`). Precedence 5.
    /// A chained `..` is one concatenation of all its operands rather than
    /// two-operand concatenations with intermediate strings between them.
    fn parse_concat(&mut self) -> Result<Expr> {
        let first = self.parse_addition()?;
        let Some(token) = self.input.try_pop(TokenKind::DotDot)? else {
            return Ok(first);
        };
        // A flattened chain reports its first operator, as Lua does when it
        // merges the right-associative concatenation instructions.
        let line = token.line;
        let mut operands = vec![first, self.parse_addition()?];
        while self.input.try_pop(TokenKind::DotDot)?.is_some() {
            operands.push(self.parse_addition()?);
        }
        if operands.len() > usize::from(MAX_EXPRESSIONS) {
            return Err(self.error_here(SyntaxError::TooManyExpressions));
        }
        Ok(Expr::new(ExprKind::Concat { operands, line }))
    }

    /// Parses an addition expression (`+`, `-`). Precedence 4.
    fn parse_addition(&mut self) -> Result<Expr> {
        let mut chain = ChainBuilder::new(self.parse_multiplication()?);
        loop {
            let op = match self.input.peek_type()? {
                TokenKind::Plus => ArithOp::Add,
                TokenKind::Minus => ArithOp::Sub,
                _ => break,
            };
            let line = self.input.next()?.line;
            let rhs = self.parse_multiplication()?;
            chain.push(self, op, rhs, line)?;
        }
        Ok(chain.finish())
    }

    /// Parses a multiplication expression (`*`, `/`, `%`). Precedence 3.
    fn parse_multiplication(&mut self) -> Result<Expr> {
        let mut chain = ChainBuilder::new(self.parse_unary()?);
        loop {
            let op = match self.input.peek_type()? {
                TokenKind::Star => ArithOp::Mul,
                TokenKind::Slash => ArithOp::Div,
                TokenKind::Mod => ArithOp::Mod,
                _ => break,
            };
            let line = self.input.next()?.line;
            let rhs = self.parse_unary()?;
            chain.push(self, op, rhs, line)?;
        }
        Ok(chain.finish())
    }

    /// Parses a unary expression (`not`, `#`, `-`). Precedence 2.
    fn parse_unary(&mut self) -> Result<Expr> {
        self.with_syntax_level(1, Self::parse_unary_inner)
    }

    fn parse_unary_inner(&mut self) -> Result<Expr> {
        let token_type = self.input.peek_type()?;
        if !matches!(
            token_type,
            TokenKind::Not | TokenKind::Hash | TokenKind::Minus
        ) {
            return self.parse_pow();
        }
        let line = self.input.next()?.line;
        let operand = self.parse_unary()?;
        Ok(match token_type {
            TokenKind::Not => Expr::new(ExprKind::Not {
                operand: Box::new(operand),
                line,
            }),
            TokenKind::Hash => Expr::new(ExprKind::Len {
                operand: Box::new(operand),
                line,
            }),
            _ => self.negate(operand, line)?,
        })
    }

    /// Negation, folding a number literal operand into its negated literal.
    /// A folded constant keeps its operand's stamp; a refused fold (negative
    /// zero, NaN, a full pool) stays a charged negation.
    fn negate(&mut self, operand: Expr, line: u32) -> Result<Expr> {
        if let ExprKind::Num(idx) = operand.kind {
            let value = -self.number_literal(idx)?;
            if let Some(folded) = self.try_fold_literal(value)? {
                return Ok(Expr::new(ExprKind::Num(folded)));
            }
        }
        Ok(Expr::new(ExprKind::Neg {
            operand: Box::new(operand),
            line,
        }))
    }

    /// Parse an exponentiation expression (`^`). Right-associative, Precedence 1.
    fn parse_pow(&mut self) -> Result<Expr> {
        let base = self.parse_primary()?;
        if let Some(token) = self.input.try_pop(TokenKind::Caret)? {
            let exponent = self.parse_unary()?;
            // `^` is deliberately never constant-folded, unlike `+ - * / %`.
            // The runtime `pow` is the in-crate correctly rounded one
            // (`crmath::pow`), identical on every host, so folding would be
            // bit-safe; it stays unfolded because a folded literal is free
            // while a runtime `^` is charged as an instruction, and folding
            // would move cost charges.
            return Ok(Expr::new(ExprKind::Pow {
                base: Box::new(base),
                exponent: Box::new(exponent),
                line: token.line,
            }));
        }
        Ok(base)
    }

    /// Parses a 'primary' expression. See `parse_prefix_exp` and `parse_expr_base` for details.
    fn parse_primary(&mut self) -> Result<Expr> {
        match self.input.peek_type()? {
            TokenKind::Identifier | TokenKind::LParen => Ok(self.parse_prefix_exp()?.into_expr()),
            _ => self.parse_expr_base(),
        }
    }

    /// Parses a `prefix expression`. Prefix expressions are the expressions
    /// which can appear on the left side of a function call, table index, or
    /// field access.
    #[hotpath::measure]
    pub(super) fn parse_prefix_exp(&mut self) -> Result<Prefix> {
        let tok = self.input.next()?;
        let base = match tok.typ {
            TokenKind::Identifier => {
                let text = self.get_text(tok)?;
                self.reject_environment_name(text, tok.start)?;
                let variable = self.parse_prefix_identifier(text)?;
                PrefixBase::Var(variable, self.input.last_line())
            }
            TokenKind::LParen => {
                let inner = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                if inner.is_multi() {
                    PrefixBase::Paren(Expr::new(ExprKind::Paren(Box::new(inner))))
                } else {
                    PrefixBase::Paren(inner)
                }
            }
            _ => {
                return Err(self.err_near(tok, |found| SyntaxError::UnexpectedSymbol { found }));
            }
        };
        let mut prefix = Prefix {
            base,
            suffixes: Vec::new(),
        };
        self.parse_prefix_extension(&mut prefix)?;
        Ok(prefix)
    }

    /// Parses every extension of a prefix expression: field accesses, table
    /// indexes, and function/method calls, in a loop like reference Lua's
    /// `suffixedexp`. A suffix chain is flat, so its length adds no syntax
    /// depth. Parsing the contents of an index or call charges a level while
    /// those contents are active, in addition to their expression levels.
    #[hotpath::measure]
    fn parse_prefix_extension(&mut self, prefix: &mut Prefix) -> Result<()> {
        loop {
            // These suffixes retain this loop and the suffix parser while
            // descending into expressions. Keep their previous depth charge:
            // without it, nested calls and indexes allow more native frames
            // per syntax-budget tick than parenthesized expressions do. The
            // charge size and its reason live on SYNTAX_SUFFIX_COST.
            let nested = matches!(
                self.input.peek_type()?,
                TokenKind::LSquare | TokenKind::LParen | TokenKind::LCurly | TokenKind::Colon
            );
            let result = if nested {
                self.with_syntax_level(SYNTAX_SUFFIX_COST, Self::parse_prefix_suffix)
            } else {
                self.parse_prefix_suffix()
            };
            match result? {
                Some(suffix) => prefix.suffixes.push(suffix),
                None => break,
            }
        }
        Ok(())
    }

    /// Parses one suffix, or returns `None` (consuming nothing) when the next
    /// token does not start a suffix. Each suffix is stamped where the
    /// emitting parser read it: a field or index at its last token, a call
    /// at the line its arguments open on, a method lookup at its name.
    fn parse_prefix_suffix(&mut self) -> Result<Option<Suffix>> {
        let suffix = match self.input.peek_type()? {
            TokenKind::Dot => {
                self.input.next()?;
                let field_name = self.expect_identifier()?;
                let name = self.find_or_add_string(field_name)?;
                Suffix::Field {
                    name,
                    line: self.input.last_line(),
                }
            }
            TokenKind::LSquare => {
                self.input.next()?;
                let key = self.parse_expr()?;
                self.expect(TokenKind::RSquare)?;
                Suffix::Index {
                    key,
                    line: self.input.last_line(),
                }
            }
            TokenKind::LParen | TokenKind::LiteralString | TokenKind::LCurly => {
                let line = self.input.peek()?.line;
                let args = self.parse_call_args()?;
                self.check_call_width(&args, 0)?;
                Suffix::Call(CallSuffix { args, line })
            }
            TokenKind::Colon => {
                let line = self.input.peek()?.line;
                self.input.next()?; // consume ':'
                let method_name = self.expect_identifier()?;
                let name = self.find_or_add_string(method_name)?;
                let lookup_line = self.input.last_line();
                let args = self.parse_call_args()?;
                // The receiver is an implicit first argument.
                self.check_call_width(&args, 1)?;
                Suffix::Method(MethodSuffix {
                    name,
                    lookup_line,
                    args,
                    line,
                })
            }
            _ => return Ok(None),
        };
        Ok(Some(suffix))
    }

    /// A call's fixed argument count is checked when its argument list
    /// closes; an expandable final argument has no fixed width.
    fn check_call_width(&self, args: &[Expr], implicit: usize) -> Result<()> {
        if args.last().is_some_and(Expr::is_multi) {
            return Ok(());
        }
        self.checked_fixed_arg_count(args.len(), implicit)?;
        Ok(())
    }

    /// Parses a 'base' expression, after eliminating any operators. This can be:
    /// * A literal number
    /// * A literal string
    /// * A function definition
    /// * One of the keywords `nil`, `false` or `true
    /// * A table constructor
    #[hotpath::measure]
    fn parse_expr_base(&mut self) -> Result<Expr> {
        let tok = self.input.next()?;
        Ok(match tok.typ {
            TokenKind::LCurly => self.parse_table(tok.line)?,
            TokenKind::LiteralNumber => {
                // The lexer determines token boundaries; this shared parser
                // owns conversion and validates the complete numeral.
                let text = self.input.source_bytes(tok.range());
                let number = crate::numeral::parse_lua_numeral(text)
                    .ok_or_else(|| self.error_at(SyntaxError::BadNumber, tok.start))?;
                Expr::new(ExprKind::Num(self.find_or_add_number(number)?))
            }
            TokenKind::LiteralString => self.literal_string(tok)?,
            TokenKind::Function => {
                let (nested, _) = self.parse_fndef(None, false)?;
                Expr::new(ExprKind::Function {
                    nested,
                    line: tok.line,
                })
            }
            TokenKind::Nil => Expr::new(ExprKind::Nil),
            TokenKind::False => Expr::new(ExprKind::False),
            TokenKind::True => Expr::new(ExprKind::True),
            TokenKind::DotDotDot => {
                if !self.funcs.chunk.is_vararg {
                    return Err(self.error_here(SyntaxError::VarargOutsideVarargFunction));
                }
                Expr::new(ExprKind::Vararg { line: tok.line })
            }
            _ => {
                return Err(self.err_near(tok, |found| SyntaxError::UnexpectedSymbol { found }));
            }
        })
    }
}

/// A same-precedence arithmetic chain under construction. While every
/// operand so far has folded into one number literal, a further literal
/// operand folds too, exactly when the emitting parser's last two
/// instructions were both literal pushes.
struct ChainBuilder {
    first: Expr,
    rest: Vec<ArithLink>,
}

impl ChainBuilder {
    fn new(first: Expr) -> Self {
        Self {
            first,
            rest: Vec::new(),
        }
    }

    fn push(&mut self, parser: &mut Parser<'_>, op: ArithOp, rhs: Expr, line: u32) -> Result<()> {
        if self.rest.is_empty()
            && op.foldable()
            && let (ExprKind::Num(lhs), ExprKind::Num(rhs_idx)) = (&self.first.kind, &rhs.kind)
        {
            let value = (op.function())(
                parser.number_literal(*lhs)?,
                parser.number_literal(*rhs_idx)?,
            );
            if let Some(folded) = parser.try_fold_literal(value)? {
                self.first = Expr::new(ExprKind::Num(folded));
                return Ok(());
            }
        }
        self.rest.push(ArithLink { op, rhs, line });
        Ok(())
    }

    fn finish(self) -> Expr {
        if self.rest.is_empty() {
            self.first
        } else {
            Expr::new(ExprKind::Arith(Box::new(ArithChain {
                first: self.first,
                rest: self.rest,
            })))
        }
    }
}
