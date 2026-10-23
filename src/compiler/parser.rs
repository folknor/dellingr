#![deny(clippy::cast_possible_truncation)]

use std::sync::Arc;

use super::Bytecode;
use super::Result;
use super::UpvalueDesc;
use super::ast::{Expr, ExprKind, FunctionBody, LocalDecl, LocalId, Stmt};
use super::codegen;
use super::error::Error;
use super::error::ErrorKind;
use super::error::SyntaxError;
use super::lexer::TokenStream;
use super::token::Token;
use super::token::TokenKind;
use crate::det_hash::{IndexMap, IndexSet};
use crate::error::{InternalFault, ParserFault};
use crate::instr::Builtin;
use crate::limits::{MAX_FIXED_ARGS, MAX_LITERALS, MAX_LOCALS, MAX_SOURCE_BYTES, MAX_SYNTAX_DEPTH};

use state::FunctionStack;

mod expr;
mod func;
mod state;
mod stmt;
mod table;
mod upvalue;

/// Tracks the current state, to make parsing easier.
#[derive(Debug)]
struct Parser<'a> {
    // The token stream owns consumed-token line state, so the parser does not
    // keep a second line field or update it for each token.
    input: TokenStream<'a>,
    /// Every active function, outermost first; field access selects the last.
    funcs: FunctionStack<'a>,
    source_name: Option<Arc<[u8]>>,
    /// Current nesting depth across recursive parser entry points.
    syntax_depth: u32,
}

/// Ordered literal storage for one chunk while parsing.
///
/// The string set becomes the bytecode pool at chunk completion. The number
/// vector stays in the chunk for constant folding, while its map stores only
/// equality keys and indices.
#[derive(Debug, Default)]
struct LiteralPoolIndices {
    strings: IndexSet<Vec<u8>>,
    numbers: IndexMap<NumberLiteralKey, u16>,
}

/// Key for number literals under Lua numeric equality.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct NumberLiteralKey(u64);

/// The upvalue-close boundary of one loop.
#[derive(Debug)]
struct LoopContext {
    close_slot: u8,
    /// `FuncState::captures` length when the loop scope was entered.
    capture_mark: usize,
}

/// A name resolved as a variable.
#[derive(Clone, Copy, Debug)]
enum Variable {
    Local { reg: u8, id: LocalId },
    Upval(u8),
    Global(u16),
    Builtin(Builtin),
}

/// Parses Lua source bytes into an unfinalized `Bytecode` tree; the compiler's
/// `compile` finalizes it.
#[hotpath::measure]
pub(super) fn parse(source: &[u8], source_name: Option<Arc<[u8]>>) -> Result<Bytecode> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::without_location(ErrorKind::SourceSizeExceeded {
            size: source.len(),
            limit: MAX_SOURCE_BYTES,
        })
        .with_source_name(source_name));
    }

    let parser = Parser {
        input: TokenStream::new(source),
        funcs: FunctionStack::default(),
        source_name: source_name.clone(),
        syntax_depth: 0,
    };
    parser
        .parse_all()
        .map_err(|error| error.with_source_name(source_name))
}

/// Pre-finalize compiler output for unit tests that inspect what `finalize`
/// rewrites.
#[cfg(test)]
pub(super) fn parse_unfinalized(source: &str) -> Result<Bytecode> {
    parse(source.as_bytes(), None)
}

impl<'a> Parser<'a> {
    // Helper functions

    /// Creates a new local slot in the current block, declaring it.
    /// Fails if we have exceeded the maximum number of locals.
    fn add_local(&mut self, name: &'a str) -> Result<LocalId> {
        if self.funcs.locals.len() == MAX_LOCALS {
            Err(self.error_here(SyntaxError::TooManyLocals))
        } else {
            let id = self.declare(name)?;
            let non_param_locals = self.funcs.locals.len() - self.funcs.chunk.num_params as usize;
            let count =
                self.bounded_u8(non_param_locals, MAX_LOCALS, SyntaxError::TooManyLocals)?;
            self.funcs.chunk.num_locals = self.funcs.chunk.num_locals.max(count);
            Ok(id)
        }
    }

    /// Records a declaration for a new active local.
    fn declare(&mut self, name: &'a str) -> Result<LocalId> {
        let id = LocalId::try_from(self.funcs.decls.len())
            .map_err(|_| self.error_here(SyntaxError::TooManyLocals))?;
        self.funcs.decls.push(LocalDecl::default());
        self.funcs.locals.push(name);
        self.funcs.local_ids.push(id);
        Ok(id)
    }

    /// Checks whether this scope can accommodate more locals without changing it.
    fn ensure_local_capacity(&self, additional: usize) -> Result<()> {
        if additional > MAX_LOCALS - self.funcs.locals.len() {
            Err(self.error_here(SyntaxError::TooManyLocals))
        } else {
            Ok(())
        }
    }

    /// Constructs an error of the given kind at the current position.
    #[must_use]
    fn error_here(&self, kind: impl Into<ErrorKind>) -> Error {
        let pos = self.input.pos();
        self.error_at(kind, pos)
    }

    /// Constructs an error of the given kind and position.
    #[must_use]
    fn error_at(&self, kind: impl Into<ErrorKind>, pos: usize) -> Error {
        let (line, column) = self.input.line_and_column(pos);
        Error::new(kind, line, column)
    }

    /// Checks an inclusive semantic bound and the u8 width together.
    fn bounded_u8(&self, value: usize, limit: usize, error: SyntaxError) -> Result<u8> {
        match u8::try_from(value) {
            Ok(value_u8) if value <= limit => Ok(value_u8),
            _ => Err(self.error_here(error)),
        }
    }

    fn local_slot_count(&self) -> Result<u8> {
        self.bounded_u8(
            self.funcs.locals.len(),
            MAX_LOCALS,
            SyntaxError::TooManyLocals,
        )
    }

    fn internal_error(&self, fault: ParserFault) -> Error {
        let pos = self.input.pos();
        self.internal_error_at(fault, pos)
    }

    fn internal_error_at(&self, fault: ParserFault, pos: usize) -> Error {
        let (line, column) = self.input.line_and_column(pos);
        Error::internal_fault(InternalFault::Parser(fault)).with_location(line, column)
    }

    fn checked_fixed_arg_count(&self, explicit: usize, implicit: usize) -> Result<u8> {
        let total = explicit + implicit;
        self.bounded_u8(total, MAX_FIXED_ARGS, SyntaxError::TooManyArguments)
    }

    /// Runs recursive parsing while charging its syntax nesting cost.
    ///
    /// The depth belongs to the parser rather than a function frame, so nested
    /// function bodies share the same limit. Inlining keeps this bracket from
    /// adding another live frame to recursive expression parsing.
    #[inline(always)]
    fn with_syntax_level<T>(
        &mut self,
        cost: u32,
        parse: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if cost > MAX_SYNTAX_DEPTH.saturating_sub(self.syntax_depth) {
            return Err(self.error_here(SyntaxError::TooManySyntaxLevels));
        }
        self.syntax_depth += cost;
        let result = parse(self);
        self.syntax_depth -= cost;
        result
    }

    /// Constructs an error for when a specific `TokenKind` was expected but not found.
    #[must_use]
    fn err_unexpected(&self, token: Token, expected: TokenKind) -> Error {
        self.err_near(token, |found| SyntaxError::ExpectedToken {
            expected,
            found,
        })
    }

    /// Constructs a syntax error about `token` from the token's source bytes.
    /// An end-of-input token is always [`SyntaxError::UnexpectedEof`],
    /// whatever was being parsed, because that is the parser kind
    /// `is_recoverable` reports. (Lexer errors at the end of the source keep
    /// their own kinds.)
    #[must_use]
    fn err_near(&self, token: Token, error: impl FnOnce(Vec<u8>) -> SyntaxError) -> Error {
        let error_kind = if token.typ == TokenKind::EndOfFile {
            SyntaxError::UnexpectedEof
        } else {
            error(self.input.source_bytes(token.range()).to_vec())
        };
        self.error_at(error_kind, token.start)
    }

    /// Reference Lua's `block_follow`: the tokens that end a statement list.
    fn is_block_follow(typ: TokenKind) -> bool {
        matches!(
            typ,
            TokenKind::Else
                | TokenKind::ElseIf
                | TokenKind::End
                | TokenKind::Until
                | TokenKind::EndOfFile
        )
    }

    /// Pulls a token off the input and checks it against `expected`.
    /// Returns the token if it matches, `Err` otherwise.
    fn expect(&mut self, expected: TokenKind) -> Result<Token> {
        let token = self.input.next()?;
        if token.typ == expected {
            Ok(token)
        } else {
            Err(self.err_unexpected(token, expected))
        }
    }

    /// Expects an identifier token and returns the identifier as a string.
    fn expect_identifier(&mut self) -> Result<&'a str> {
        let token = self.expect(TokenKind::Identifier)?;
        self.get_text(token)
    }

    /// Expects a variable name and rejects `_ENV`, whose Lua 5.2 environment
    /// semantics are not implemented by this compiler.
    fn expect_variable_identifier(&mut self) -> Result<&'a str> {
        let token = self.expect(TokenKind::Identifier)?;
        let name = self.get_text(token)?;
        self.reject_environment_name(name, token.start)?;
        Ok(name)
    }

    /// Rejects `_ENV` where an identifier denotes a variable. Field names
    /// still use `expect_identifier`, since `t._ENV` is an ordinary field.
    fn reject_environment_name(&self, name: &str, pos: usize) -> Result<()> {
        if name == "_ENV" {
            Err(self.error_at(SyntaxError::UnsupportedEnvironmentName, pos))
        } else {
            Ok(())
        }
    }

    /// Expects an identifier and returns the id of its string literal.
    fn expect_identifier_id(&mut self) -> Result<u16> {
        let name = self.expect_identifier()?;
        self.find_or_add_string(name)
    }

    /// Stores a literal string and returns its index.
    fn find_or_add_string(&mut self, string: &str) -> Result<u16> {
        self.find_or_add_string_bytes(string.as_bytes())
    }

    /// Stores literal string bytes and returns its index.
    fn find_or_add_string_bytes(&mut self, bytes: &[u8]) -> Result<u16> {
        crate::limits::check_string_size(bytes.len())
            .map_err(|err| self.error_here(err.into_kind()))?;
        if let Some(index) = self.funcs.literal_indices.strings.get_index_of(bytes) {
            return u16::try_from(index).map_err(|_| self.error_here(SyntaxError::TooManyStrings));
        }

        let i = self.funcs.literal_indices.strings.len();
        // MAX_LITERALS keeps indices within u16, so the conversion cannot fail.
        let index = match u16::try_from(i) {
            Ok(index) if i < MAX_LITERALS => index,
            _ => return Err(self.error_here(SyntaxError::TooManyStrings)),
        };
        self.funcs.literal_indices.strings.insert(bytes.to_vec());
        Ok(index)
    }

    /// Stores a literal number and returns its index.
    fn find_or_add_number(&mut self, num: f64) -> Result<u16> {
        let key = number_literal_key(num);
        if let Some(index) = key.and_then(|key| self.funcs.literal_indices.numbers.get(&key)) {
            return Ok(*index);
        }

        let i = self.funcs.chunk.number_literals.len();
        // MAX_LITERALS keeps indices within u16, so the conversion cannot fail.
        let index = match u16::try_from(i) {
            Ok(index) if i < MAX_LITERALS => index,
            _ => return Err(self.error_here(SyntaxError::TooManyNumbers)),
        };
        self.funcs.chunk.number_literals.push(num);
        if let Some(key) = key {
            self.funcs.literal_indices.numbers.insert(key, index);
        }
        Ok(index)
    }

    /// The value of a number literal by pool index.
    fn number_literal(&self, idx: u16) -> Result<f64> {
        self.funcs
            .chunk
            .number_literals
            .get(usize::from(idx))
            .copied()
            .ok_or_else(|| self.internal_error(ParserFault::FoldedLiteralOutOfRange))
    }

    /// Folding must preserve signed zero and NaN behavior, and a full pool
    /// must leave otherwise valid source compilable using runtime arithmetic.
    ///
    /// Folding is a deliberate, documented exception to "naming an intermediate
    /// must not make a script more expensive": the folded expression charges
    /// nothing, while the same arithmetic over locals is charged at runtime. The
    /// operand literals stay in `number_literals` as dead entries (they count
    /// toward the literal limit, are serialized into snapshots and occupy
    /// runtime literal slots). They are not compacted away because that would
    /// change snapshot contents for identical source, which is replay-visible.
    fn try_fold_literal(&mut self, value: f64) -> Result<Option<u16>> {
        if value.is_nan() || (value == 0.0 && value.is_sign_negative()) {
            return Ok(None);
        }
        match self.find_or_add_number(value) {
            Ok(index) => Ok(Some(index)),
            Err(error) => match error.kind() {
                ErrorKind::SyntaxError(SyntaxError::TooManyNumbers) => Ok(None),
                _ => Err(error),
            },
        }
    }

    /// Takes the decoded bytes retained for a short-string token by the lexer.
    fn get_literal_string_contents(&mut self, tok: Token) -> Result<Vec<u8>> {
        use super::lexer::LiteralStringContents;

        if tok.typ != TokenKind::LiteralString || tok.len < 2 {
            return Err(self.internal_error_at(ParserFault::InvalidStringToken, tok.start));
        }

        match self.input.take_literal_string_contents(tok.start) {
            Some(LiteralStringContents::Decoded(contents)) => Ok(contents),
            Some(LiteralStringContents::SyntaxError { kind, pos }) => Err(self.error_at(kind, pos)),
            Some(LiteralStringContents::StringSizeExceeded { size, limit }) => {
                // Public parsing caps source bytes at MAX_STRING_BYTES, and
                // decoding a quoted literal cannot grow beyond its source.
                // This defensive branch is unreachable for admitted source,
                // so its next-token location cannot misattribute a user error.
                Err(self.error_here(ErrorKind::StringSizeExceeded { size, limit }))
            }
            None => Err(self.internal_error_at(ParserFault::MissingStringContents, tok.start)),
        }
    }

    /// The source text of an identifier or numeral token. The byte lexer only
    /// builds those tokens from ASCII, so the conversion cannot fail on lexer
    /// output; a failure is a lexer bug reported as an internal error.
    fn get_text(&self, token: Token) -> Result<&'a str> {
        std::str::from_utf8(self.input.source_bytes(token.range()))
            .map_err(|_| self.internal_error_at(ParserFault::TokenTextNotUtf8, token.start))
    }

    /// Starts a block at the next available local slot.
    fn level_up(&mut self) {
        let base = self.funcs.locals.len();
        let mark = self.funcs.captures.len();
        self.funcs.block_starts.push((base, mark));
    }

    /// Whether a nested function captured a local at or above `slot` since
    /// the capture log had length `mark`. Only such a capture can leave an
    /// open upvalue for a scope to close.
    fn captured_since(&self, mark: usize, slot: usize) -> bool {
        self.funcs.captures[mark..]
            .iter()
            .any(|&captured| usize::from(captured) >= slot)
    }

    /// Whether the innermost loop's body locals may hold an open upvalue at
    /// the end of an iteration.
    fn loop_body_captured(&self) -> bool {
        self.funcs.loops.last().is_none_or(|context| {
            self.captured_since(context.capture_mark, usize::from(context.close_slot))
        })
    }

    /// Discards exactly the locals declared in this block and decides whether
    /// leaving it must close upvalues, and from which register.
    fn level_down(&mut self) -> Result<Option<u8>> {
        let Some((block_base, mark)) = self.funcs.block_starts.pop() else {
            return Err(self.internal_error(ParserFault::BlockExitWithoutEntry));
        };
        let before = self.funcs.locals.len();
        self.funcs.locals.truncate(block_base);
        self.funcs.local_ids.truncate(block_base);
        if block_base < before && self.captured_since(mark, block_base) {
            let slot = self.bounded_u8(block_base, MAX_LOCALS, SyntaxError::TooManyLocals)?;
            return Ok(Some(slot));
        }
        Ok(None)
    }

    /// Called when entering a loop body whose locals start at `close_slot`.
    fn enter_loop(&mut self, close_slot: u8) {
        let capture_mark = self.funcs.captures.len();
        self.funcs.loops.push(LoopContext {
            close_slot,
            capture_mark,
        });
    }

    /// Called when exiting a loop: its breaks close upvalues from the
    /// returned register when anything in the loop scope was captured.
    fn exit_loop(&mut self) -> Result<Option<u8>> {
        let Some(context) = self.funcs.loops.pop() else {
            return Err(self.internal_error(ParserFault::LoopExitWithoutEntry));
        };
        Ok(self
            .captured_since(context.capture_mark, usize::from(context.close_slot))
            .then_some(context.close_slot))
    }

    // Actual parsing

    /// The main entry point for the parser. This parses the entire input.
    #[hotpath::measure]
    fn parse_all(mut self) -> Result<Bytecode> {
        // The top-level chunk is a vararg function (can receive command-line args)
        let c = self.parse_chunk(&[], true)?;
        let token = self.input.next()?;
        if !self.funcs.frames.is_empty() {
            return Err(self.internal_error(ParserFault::CompletedSourceRetainsFunctionFrames));
        }
        if let TokenKind::EndOfFile = token.typ {
            Ok(c)
        } else {
            Err(self.err_unexpected(token, TokenKind::EndOfFile))
        }
    }

    /// Parses a function body and generates its code.
    #[hotpath::measure]
    fn parse_chunk(&mut self, params: &[&'a str], is_vararg: bool) -> Result<Bytecode> {
        let num_params = self.bounded_u8(params.len(), MAX_LOCALS, SyntaxError::TooManyLocals)?;
        // The root is a real frame too; no placeholder chunk participates in
        // name lookup or the function-depth limit.
        self.funcs.frames.push(state::FuncState::new(
            self.source_name.clone(),
            num_params,
            is_vararg,
        ));

        for &param in params {
            self.declare(param)?;
        }

        let (body, _) = self.parse_statements()?;
        self.finish_chunk(num_params, body)
    }

    /// Closes the innermost function frame and generates its code. Kept out
    /// of `parse_chunk`, which stays live on the native stack through every
    /// nested function, so the finished chunk and the code generator occupy
    /// stack only while one function is being finished.
    #[inline(never)]
    fn finish_chunk(&mut self, num_params: u8, body: Vec<Stmt>) -> Result<Bytecode> {
        // The implicit return belongs to the block terminator, even though
        // the enclosing parser has not consumed that token yet.
        let end_line = self.input.peek()?.line;

        // Copy upvalues to chunk
        self.funcs.chunk.upvalues = self.funcs.upvalues.iter().map(|(_, desc)| *desc).collect();

        let Some(finished) = self.funcs.frames.pop() else {
            return Err(self.internal_error(ParserFault::ChunkCompletionWithoutFunctionFrame));
        };
        if !finished.loops.is_empty() || !finished.block_starts.is_empty() {
            return Err(self.internal_error(ParserFault::CompletedFunctionRetainsScopes));
        }
        let mut chunk = finished.chunk;
        // Keep one copy of each string while parsing, then build the canonical
        // bytecode pool in the same first-insertion order.
        chunk.string_literals = finished.literal_indices.strings.into_iter().collect();
        let function = FunctionBody {
            num_params,
            body,
            end_line,
            locals: finished.decls,
        };
        codegen::generate(&mut chunk, &function).map_err(|error| match error {
            codegen::CodegenError::JumpTooFar => self.error_here(SyntaxError::JumpTooFar),
            codegen::CodegenError::TooManyRegisters { count, line } => Error::new(
                SyntaxError::TooManyRegisters { count },
                if line == 0 {
                    self.input.last_line()
                } else {
                    line
                },
                0,
            ),
            codegen::CodegenError::Internal(fault) => self.internal_error(*fault),
        })?;
        Ok(chunk)
    }

    /// Parses 0 or more statements, possibly separated by semicolons.
    /// Also returns whether the block's own last statement was a `return`,
    /// excluding returns inside nested statements (including `do` blocks).
    #[hotpath::measure]
    fn parse_statements(&mut self) -> Result<(Vec<Stmt>, bool)> {
        self.with_syntax_level(1, Self::parse_statements_inner)
    }

    fn parse_statements_inner(&mut self) -> Result<(Vec<Stmt>, bool)> {
        let mut stmts = Vec::new();
        loop {
            // Every statement parser runs through `push_statement`, so this
            // frame, which stays live through every nested block and
            // function, holds no statement temporaries of its own.
            let parse: fn(&mut Self) -> Result<Stmt> = match self.input.peek_type()? {
                TokenKind::Identifier | TokenKind::LParen => Self::parse_assign_or_call,
                TokenKind::If => Self::parse_if_arm,
                TokenKind::While => Self::parse_while,
                TokenKind::Repeat => Self::parse_repeat,
                TokenKind::Do => Self::parse_do,
                TokenKind::Local => Self::parse_locals,
                TokenKind::For => Self::parse_for,
                TokenKind::Function => Self::parse_fndecl,
                TokenKind::Semi => {
                    self.input.next()?;
                    continue;
                }
                TokenKind::Return => {
                    self.push_statement(&mut stmts, Self::parse_return)?;
                    break Ok((stmts, true));
                }
                TokenKind::Break => {
                    self.input.next()?; // consume 'break' keyword
                    if self.funcs.loops.is_empty() {
                        break Err(self.error_here(SyntaxError::BreakOutsideLoop));
                    }
                    stmts.push(Stmt::Break);
                    continue;
                }
                typ if Self::is_block_follow(typ) => break Ok((stmts, false)),
                // Reference Lua parses any other token as the start of an
                // expression statement, which fails at its first token.
                _ => {
                    let token = self.input.next()?;
                    break Err(
                        self.err_near(token, |found| SyntaxError::UnexpectedSymbol { found })
                    );
                }
            };
            self.push_statement(&mut stmts, parse)?;
        }
    }

    #[inline(never)]
    fn push_statement(
        &mut self,
        stmts: &mut Vec<Stmt>,
        parse: fn(&mut Self) -> Result<Stmt>,
    ) -> Result<()> {
        stmts.push(parse(self)?);
        Ok(())
    }

    /// Resolves a variable's name to a local, upvalue, builtin or global.
    fn parse_prefix_identifier(&mut self, name: &'a str) -> Result<Variable> {
        let level = self.funcs.frames.len() - 1;
        if let Some(binding) = self.resolve_in(level, name)? {
            return Ok(match binding {
                UpvalueDesc::Local(reg) => {
                    let id = self
                        .funcs
                        .local_ids
                        .get(usize::from(reg))
                        .copied()
                        .ok_or_else(|| self.internal_error(ParserFault::LocalWithoutDeclaration))?;
                    Variable::Local { reg, id }
                }
                UpvalueDesc::Upvalue(index) => Variable::Upval(index),
            });
        }

        // Check if it's a well-known builtin for fast access
        if let Some(builtin) = Builtin::from_name(name) {
            return Ok(Variable::Builtin(builtin));
        }

        // Otherwise it's a regular global
        let i = self.find_or_add_string(name)?;
        Ok(Variable::Global(i))
    }

    /// Parses a literal string token into its pool index.
    fn literal_string(&mut self, tok: Token) -> Result<Expr> {
        let text = self.get_literal_string_contents(tok)?;
        let idx = self.find_or_add_string_bytes(&text)?;
        Ok(Expr::new(ExprKind::Str(idx)))
    }

    /// Parses function-call args.
    ///
    /// Lua allows calls with parenthesized arguments, a single table constructor,
    /// or a single literal string: `f(...)`, `f{...}`, and `f"..."`.
    #[hotpath::measure]
    fn parse_call_args(&mut self) -> Result<Vec<Expr>> {
        let tok = self.input.next()?;
        match tok.typ {
            TokenKind::LParen => self.parse_parenthesized_call_args(),
            TokenKind::LiteralString => Ok(vec![self.literal_string(tok)?]),
            TokenKind::LCurly => Ok(vec![self.parse_table(tok.line)?]),
            _ => Err(
                self.err_near(tok, |found| SyntaxError::FunctionArgumentsExpected {
                    found,
                }),
            ),
        }
    }

    /// Parses parenthesized function-call args after the opening paren.
    #[hotpath::measure]
    fn parse_parenthesized_call_args(&mut self) -> Result<Vec<Expr>> {
        let args = if self.input.check_type(TokenKind::RParen)? {
            Vec::new()
        } else {
            self.parse_explist()?
        };
        self.expect(TokenKind::RParen)?;
        Ok(args)
    }
}

/// Finds the index of the last local entry which matches `name`.
#[must_use]
fn find_last_local(locals: &[&str], name: &str) -> Option<usize> {
    let mut i = locals.len();
    while i > 0 {
        i -= 1;
        if locals[i] == name {
            return Some(i);
        }
    }

    None
}

/// Returns a stable lookup key for values equal under the literal pool's `==` semantics.
/// NaNs intentionally have no key because `NaN != NaN`; signed zeroes share one key.
fn number_literal_key(number: f64) -> Option<NumberLiteralKey> {
    if number.is_nan() {
        return None;
    }

    let bits = number.to_bits();
    if bits & !(1_u64 << 63) == 0 {
        Some(NumberLiteralKey(0))
    } else {
        Some(NumberLiteralKey(bits))
    }
}

/// Thread stack for unit tests that parse at the syntax-budget limit: the
/// 2 MiB docs/limits.md promises for parsing in every repository profile,
/// set explicitly so `RUST_MIN_STACK` cannot loosen it. Instrumented
/// `hotpath` builds wrap parser functions in timing guards and get more.
#[cfg(all(test, not(feature = "hotpath")))]
pub(crate) const DEEP_PARSE_TEST_STACK_BYTES: usize = 2 << 20;
#[cfg(all(test, feature = "hotpath"))]
pub(crate) const DEEP_PARSE_TEST_STACK_BYTES: usize = 8 << 20;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod line_attribution_tests;
