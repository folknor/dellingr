#![deny(clippy::cast_possible_truncation)]

use std::sync::Arc;

use super::Bytecode;
use super::Instr;
use super::Result;
use super::UpvalueDesc;
use super::error::Error;
use super::error::ErrorKind;
use super::error::SyntaxError;
use super::exp_desc::ExpDesc;
use super::exp_desc::PlaceExp;
use super::exp_desc::PrefixExp;
use super::exp_desc::TailKind;
use super::lexer::TokenStream;
use super::token::Token;
use super::token::TokenType;
use crate::det_hash::{IndexMap, IndexSet};
use crate::error::ParserFault;
use crate::instr::{ArgCount, Builtin, RetCount};
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
    source_name: Option<Arc<str>>,
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

/// Tracks the break jumps and upvalue-close boundary for one loop.
#[derive(Debug)]
struct LoopContext {
    close_slot: u8,
    /// `FuncState::captures` length when the loop scope was entered.
    capture_mark: usize,
    break_jumps: Vec<usize>,
    /// The CloseUpvalues emitted before each break. A capture textually after
    /// a break can still be live at it on a later iteration, so whether these
    /// are needed is decided only at loop exit, once the whole body is known.
    break_closes: Vec<usize>,
}

/// Parses Lua source code into a `Bytecode`.
pub(super) fn parse_str(source: &str) -> Result<Bytecode> {
    parse_str_named(source, None)
}

/// Parses Lua source code into a `Bytecode` with an optional source name.
#[hotpath::measure]
pub(super) fn parse_str_named(source: &str, source_name: Option<String>) -> Result<Bytecode> {
    let source_name = source_name.map(Arc::<str>::from);
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

impl<'a> Parser<'a> {
    // Helper functions

    /// Creates a new local slot in the current block.
    /// Fails if we have exceeded the maximum number of locals.
    fn add_local(&mut self, name: &'a str) -> Result<()> {
        if self.funcs.locals.len() == MAX_LOCALS {
            Err(self.error_here(SyntaxError::TooManyLocals))
        } else {
            self.funcs.locals.push(name);
            let non_param_locals = self.funcs.locals.len() - self.funcs.chunk.num_params as usize;
            let count =
                self.bounded_u8(non_param_locals, MAX_LOCALS, SyntaxError::TooManyLocals)?;
            self.funcs.chunk.num_locals = self.funcs.chunk.num_locals.max(count);
            Ok(())
        }
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

    /// Checks an inclusive semantic bound and the instruction operand width together.
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
        self.error_here(ErrorKind::InternalError(fault.into_message().to_owned()))
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

    fn checked_jump_offset(&self, from: usize, target: usize) -> Result<i16> {
        let delta = target as isize - (from as isize + 1);
        i16::try_from(delta).map_err(|_| self.error_here(SyntaxError::JumpTooFar))
    }

    fn patch_jump(
        &mut self,
        from: usize,
        target: usize,
        constructor: impl FnOnce(i16) -> Instr,
    ) -> Result<()> {
        self.funcs.chunk.code[from] = constructor(self.checked_jump_offset(from, target)?);
        // A patched landing site must keep executing exactly the instruction
        // it points at; see `fold_barrier`.
        self.funcs.fold_barrier = self.funcs.fold_barrier.max(target);
        Ok(())
    }

    /// Constructs an error for when a specific `TokenType` was expected but not found.
    #[must_use]
    fn err_unexpected(&self, token: Token, expected: TokenType) -> Error {
        let error_kind = if token.typ == TokenType::EndOfFile {
            SyntaxError::UnexpectedEof
        } else {
            let got = self.describe_token(&token);
            let exp = Self::describe_token_type(expected);
            SyntaxError::expected_token(exp, got)
        };
        self.error_at(error_kind, token.start)
    }

    /// Returns a human-readable description of the given token.
    fn describe_token(&self, token: &Token) -> String {
        match token.typ {
            TokenType::Identifier | TokenType::LiteralNumber | TokenType::LiteralString => {
                let text = self
                    .input
                    .substring(token.start..token.start + token.len as usize);
                format!("'{text}'")
            }
            TokenType::EndOfFile => "<eof>".to_string(),
            other => format!("'{}'", Self::describe_token_type(other)),
        }
    }

    /// Returns the display name for a TokenType.
    fn describe_token_type(typ: TokenType) -> &'static str {
        typ.spelling()
    }

    /// Pulls a token off the input and checks it against `expected`.
    /// Returns the token if it matches, `Err` otherwise.
    fn expect(&mut self, expected: TokenType) -> Result<Token> {
        let token = self.input.next()?;
        if token.typ == expected {
            Ok(token)
        } else {
            Err(self.err_unexpected(token, expected))
        }
    }

    /// Expects an identifier token and returns the identifier as a string.
    fn expect_identifier(&mut self) -> Result<&'a str> {
        let token = self.expect(TokenType::Identifier)?;
        let name = self.get_text(token);
        Ok(name)
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

    /// Takes the decoded bytes retained for a short-string token by the lexer.
    fn get_literal_string_contents(&mut self, tok: Token) -> Result<Vec<u8>> {
        use super::lexer::LiteralStringContents;

        if tok.typ != TokenType::LiteralString || tok.len < 2 {
            return Err(self.error_at(
                ErrorKind::InternalError(ParserFault::InvalidStringToken.into_message().to_owned()),
                tok.start,
            ));
        }

        match self.input.take_literal_string_contents(tok.start) {
            Some(LiteralStringContents::Decoded(contents)) => Ok(contents),
            Some(LiteralStringContents::SyntaxError { kind, pos }) => Err(self.error_at(kind, pos)),
            Some(LiteralStringContents::StringSizeExceeded { size, limit }) => {
                Err(self.error_here(ErrorKind::StringSizeExceeded { size, limit }))
            }
            None => Err(self.error_at(
                ErrorKind::InternalError(
                    ParserFault::MissingStringContents.into_message().to_owned(),
                ),
                tok.start,
            )),
        }
    }

    /// Gets the original source code contained by a token.
    #[must_use]
    fn get_text(&self, token: Token) -> &'a str {
        self.input.substring(token.range())
    }

    /// Starts a block at the next available local slot.
    fn level_up(&mut self) {
        let base = self.funcs.locals.len();
        let mark = self.funcs.captures.len();
        self.funcs.block_starts.push((base, mark));
    }

    /// Whether a nested function captured a local at or above `slot` since
    /// the capture log had length `mark`. Only such a capture can leave an
    /// open upvalue for a scope's CloseUpvalues to close.
    fn captured_since(&self, mark: usize, slot: usize) -> bool {
        self.funcs.captures[mark..]
            .iter()
            .any(|&captured| usize::from(captured) >= slot)
    }

    /// Whether the innermost loop's body locals may hold an open upvalue at
    /// the end of an iteration.
    fn loop_body_captured(&self) -> bool {
        self.funcs.loop_breaks.last().is_none_or(|context| {
            self.captured_since(context.capture_mark, usize::from(context.close_slot))
        })
    }

    /// Closes and discards exactly the locals declared in this block.
    fn level_down(&mut self) -> Result<()> {
        let Some((block_base, mark)) = self.funcs.block_starts.pop() else {
            return Err(self.internal_error(ParserFault::BlockExitWithoutEntry));
        };
        let before = self.funcs.locals.len();
        self.funcs.locals.truncate(block_base);
        if block_base < before && self.captured_since(mark, block_base) {
            let slot = self.bounded_u8(block_base, MAX_LOCALS, SyntaxError::TooManyLocals)?;
            self.push(Instr::close_upvalues(slot));
        }
        Ok(())
    }

    /// Adds an instruction to the output with line number tracking.
    fn push(&mut self, instr: Instr) {
        self.funcs.chunk.code.push(instr);
        self.funcs.chunk.line_info.push(self.input.last_line());
    }

    /// The values of the two trailing instructions when both are `PushNum`
    /// and no patched jump target forbids merging them (`fold_barrier`).
    fn foldable_binary_operands(&self) -> Option<(f64, f64)> {
        let first = self.funcs.chunk.code.len().checked_sub(2)?;
        if first < self.funcs.fold_barrier {
            return None;
        }
        let lhs = self.funcs.chunk.code[first];
        let rhs = self.funcs.chunk.code[first + 1];
        if lhs.opcode() != Instr::OP_PUSH_NUM || rhs.opcode() != Instr::OP_PUSH_NUM {
            return None;
        }
        Some((
            self.funcs.chunk.number_literals[lhs.bx() as usize],
            self.funcs.chunk.number_literals[rhs.bx() as usize],
        ))
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

    /// Emits an arithmetic instruction, folding `PushNum a; PushNum b; op`
    /// into one `PushNum` when both operands are number literals. The folder
    /// uses `numeral::arith_fn`, the same function the VM dispatch applies for
    /// this opcode, so the folded value is bit-exact with runtime evaluation.
    /// An unfolded instruction records `line`, the operator's line. A full literal
    /// pool falls back to emitting the instruction unfolded rather than
    /// failing a program the unfolded compiler accepted.
    fn push_arith(&mut self, instr: Instr, line: u32) -> Result<()> {
        let op = crate::numeral::arith_fn(instr.opcode()).ok_or_else(|| {
            self.internal_error(ParserFault::ArithmeticFoldRequiresFoldableOpcode)
        })?;
        if let Some((lhs, rhs)) = self.foldable_binary_operands() {
            let result = op(lhs, rhs);
            if let Some(idx) = self.try_fold_literal(result)? {
                self.funcs.chunk.code.pop();
                self.funcs.chunk.line_info.pop();
                self.replace_last_instr(Instr::push_num(idx))?;
                return Ok(());
            }
        }
        self.push_at_line(instr, line);
        Ok(())
    }

    /// Emits the branch-if-false that follows a condition, fusing it with a
    /// trailing comparison instruction when one is present. Returns the
    /// branch's code index and the constructor to patch it with (both the
    /// fused and plain forms start as offset-0 placeholders).
    ///
    /// The fuse refuses when `fold_barrier` reaches the would-be branch slot:
    /// a patched jump landing there (the `or`-skip in `if a or b < c then`)
    /// expects to pop one condition value, and the fused instruction pops
    /// two. A target at the comparison itself is fine - the fused form has
    /// the same stack effect and control flow from that entry point.
    fn push_branch_false(&mut self) -> (usize, fn(i16) -> Instr) {
        let len = self.funcs.chunk.code.len();
        if len > self.funcs.fold_barrier
            && let Some(last) = self.funcs.chunk.code.last()
        {
            let fused = Instr::fused_branch_constructor(last.opcode());
            if let Some(constructor) = fused {
                let idx = len - 1;
                self.funcs.chunk.code[idx] = constructor(0);
                return (idx, constructor);
            }
        }
        self.push(Instr::branch_false(0));
        (len, Instr::branch_false)
    }

    /// Emits unary negation, folding `PushNum v; Negate` into `PushNum -v`.
    /// Replacing in place is safe even at a jump target: the landing site
    /// still computes the same value. An emitted `Negate` records `line`, the
    /// operator's line; a folded constant keeps its operand's line.
    fn push_negate(&mut self, line: u32) -> Result<()> {
        if self.funcs.chunk.code.len() > self.funcs.fold_barrier
            && let Some(last) = self.funcs.chunk.code.last()
            && last.opcode() == Instr::OP_PUSH_NUM
        {
            let value = -self.funcs.chunk.number_literals[last.bx() as usize];
            if let Some(idx) = self.try_fold_literal(value)? {
                self.replace_last_instr(Instr::push_num(idx))?;
                return Ok(());
            }
        }
        self.push_at_line(Instr::negate(), line);
        Ok(())
    }

    /// Adds an instruction with an explicit source line.
    fn push_at_line(&mut self, instr: Instr, line: u32) {
        self.funcs.chunk.code.push(instr);
        self.funcs.chunk.line_info.push(line);
    }

    /// Overwrites the last emitted instruction in place, returning the old one.
    fn replace_last_instr(&mut self, instr: Instr) -> Result<Instr> {
        let Some(slot) = self.funcs.chunk.code.last_mut() else {
            return Err(self.internal_error(ParserFault::InstructionReplacementWithoutInstruction));
        };
        Ok(std::mem::replace(slot, instr))
    }

    /// Changes a typed final call or vararg to provide every requested tail value.
    pub(super) fn open_tail(&mut self, tail: TailKind, want: RetCount) -> Result<()> {
        let (replacement, expected_opcode) = match tail {
            TailKind::Vararg => (Instr::vararg(want), Instr::OP_VARARG),
            TailKind::FunctionCall(call) => (Instr::call(call.num_args(), want), Instr::OP_CALL),
        };
        if self.funcs.chunk.code.last().map(|instr| instr.opcode()) != Some(expected_opcode) {
            return Err(self.internal_error(ParserFault::MultiValueTailMissingInstruction));
        }
        self.replace_last_instr(replacement)?;
        Ok(())
    }

    /// Called when entering a loop to track break statements.
    fn enter_loop(&mut self, close_slot: u8) {
        let capture_mark = self.funcs.captures.len();
        self.funcs.loop_breaks.push(LoopContext {
            close_slot,
            capture_mark,
            break_jumps: Vec::new(),
            break_closes: Vec::new(),
        });
    }

    /// Called when exiting a loop. Patches all break jumps to jump to the
    /// current instruction position (the instruction after the loop).
    fn exit_loop(&mut self) -> Result<()> {
        let Some(context) = self.funcs.loop_breaks.pop() else {
            return Err(self.internal_error(ParserFault::LoopExitWithoutEntry));
        };
        if !self.captured_since(context.capture_mark, usize::from(context.close_slot)) {
            // No open upvalue can exist above the boundary. In-place NOPs keep
            // every recorded jump index valid; finalize strips and remaps them.
            for close_idx in &context.break_closes {
                self.funcs.chunk.code[*close_idx] = Instr::nop();
            }
        }
        let loop_end = self.funcs.chunk.code.len();
        for break_idx in context.break_jumps {
            // The break instruction is a Jump with placeholder offset.
            // Patch it to jump to the end of the loop.
            self.patch_jump(break_idx, loop_end, Instr::jump)?;
        }
        Ok(())
    }

    /// Records a break statement. Returns an error if not inside a loop.
    fn add_break(&mut self) -> Result<()> {
        if let Some(close_slot) = self
            .funcs
            .loop_breaks
            .last()
            .map(|context| context.close_slot)
        {
            let close_idx = self.funcs.chunk.code.len();
            self.push(Instr::close_upvalues(close_slot));
            // Record the index where we'll emit the Jump instruction.
            let idx = self.funcs.chunk.code.len();
            let Some(context) = self.funcs.loop_breaks.last_mut() else {
                return Err(self.internal_error(ParserFault::BreakEmissionLostLoopContext));
            };
            context.break_closes.push(close_idx);
            context.break_jumps.push(idx);
            // Emit a placeholder Jump that will be patched by exit_loop
            self.push(Instr::jump(0));
            Ok(())
        } else {
            Err(self.error_here(SyntaxError::BreakOutsideLoop))
        }
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
        if let TokenType::EndOfFile = token.typ {
            Ok(c)
        } else {
            Err(self.err_unexpected(token, TokenType::EndOfFile))
        }
    }

    /// Parses a `Bytecode`.
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
            self.funcs.locals.push(param);
        }

        self.parse_statements()?;
        // The implicit return belongs to the block terminator, even though
        // the enclosing parser has not consumed that token yet.
        let end_line = self.input.peek()?.line;
        self.push_at_line(Instr::ret(RetCount::Fixed(0)), end_line);

        // Copy upvalues to chunk
        self.funcs.chunk.upvalues = self.funcs.upvalues.iter().map(|(_, desc)| *desc).collect();

        let Some(finished) = self.funcs.frames.pop() else {
            return Err(self.internal_error(ParserFault::ChunkCompletionWithoutFunctionFrame));
        };
        if !finished.loop_breaks.is_empty() || !finished.block_starts.is_empty() {
            return Err(self.internal_error(ParserFault::CompletedFunctionRetainsScopes));
        }
        let mut tmp_chunk = finished.chunk;
        // Keep one copy of each string while parsing, then build the canonical
        // bytecode pool in the same first-insertion order.
        tmp_chunk.string_literals = finished.literal_indices.strings.into_iter().collect();
        Ok(tmp_chunk)
    }

    /// Parses 0 or more statements, possibly separated by semicolons.
    /// Returns whether the block's own last statement was a `return`, excluding
    /// returns inside nested statements (including `do` blocks). A true result
    /// guarantees that control cannot fall off the end of this block.
    #[hotpath::measure]
    fn parse_statements(&mut self) -> Result<bool> {
        self.with_syntax_level(1, Self::parse_statements_inner)
    }

    fn parse_statements_inner(&mut self) -> Result<bool> {
        loop {
            match self.input.peek_type()? {
                TokenType::Identifier | TokenType::LParen => {
                    self.parse_assign_or_call()?;
                }
                TokenType::If => self.parse_if_arm()?,
                TokenType::While => self.parse_while()?,
                TokenType::Repeat => self.parse_repeat()?,
                TokenType::Do => self.parse_do()?,
                TokenType::Local => self.parse_locals()?,
                TokenType::For => self.parse_for()?,
                TokenType::Function => self.parse_fndecl()?,
                TokenType::Semi => {
                    self.input.next()?;
                }
                TokenType::Return => break self.parse_return().map(|()| true),
                TokenType::Break => {
                    self.input.next()?; // consume 'break' keyword
                    self.add_break()?;
                }
                _ => break Ok(false),
            }
        }
    }

    /// Emits code to evaluate the prefix expression as a normal expression.
    fn eval_prefix_exp(&mut self, exp: &PrefixExp) {
        match exp {
            PrefixExp::FunctionCall(call) => {
                let (num_args, line) = (call.num_args(), call.line());
                self.push_at_line(Instr::call(num_args, RetCount::Fixed(1)), line);
            }
            PrefixExp::Parenthesized => (),
            PrefixExp::Place(place) => {
                self.push(place.get_instr());
            }
        }
    }

    /// Parses a variable's name. Returns Local, Upvalue, or Global.
    fn parse_prefix_identifier(&mut self, name: &'a str) -> Result<PlaceExp> {
        let level = self.funcs.frames.len() - 1;
        if let Some(binding) = self.resolve_in(level, name)? {
            return Ok(match binding {
                UpvalueDesc::Local(index) => PlaceExp::Local(index),
                UpvalueDesc::Upvalue(index) => PlaceExp::Upvalue(index),
            });
        }

        // Check if it's a well-known builtin for fast access
        if let Some(builtin) = Builtin::from_name(name) {
            return Ok(PlaceExp::Builtin(builtin));
        }

        // Otherwise it's a regular global
        let i = self.find_or_add_string(name)?;
        Ok(PlaceExp::Global(i))
    }

    /// Parses a literal string and emits bytecode to push it.
    fn push_literal_string(&mut self, tok: Token) -> Result<()> {
        let text = self.get_literal_string_contents(tok)?;
        let idx = self.find_or_add_string_bytes(&text)?;
        self.push(Instr::push_string(idx));
        Ok(())
    }

    /// Parses function-call args. Returns the number of arguments.
    ///
    /// Lua allows calls with parenthesized arguments, a single table constructor,
    /// or a single literal string: `f(...)`, `f{...}`, and `f"..."`.
    #[hotpath::measure]
    fn parse_call_args(&mut self) -> Result<(u8, ExpDesc)> {
        let tok = self.input.next()?;
        match tok.typ {
            TokenType::LParen => self.parse_parenthesized_call_args(),
            TokenType::LiteralString => {
                self.push_literal_string(tok)?;
                Ok((1, ExpDesc::Other))
            }
            TokenType::LCurly => {
                self.parse_table()?;
                Ok((1, ExpDesc::Other))
            }
            _ => Err(self.err_unexpected(tok, TokenType::LParen)),
        }
    }

    /// Parses parenthesized function-call args after the opening paren.
    /// Returns the number of arguments.
    #[hotpath::measure]
    fn parse_parenthesized_call_args(&mut self) -> Result<(u8, ExpDesc)> {
        let tup = if self.input.check_type(TokenType::RParen)? {
            (0, ExpDesc::Other)
        } else {
            self.parse_explist()?
        };
        self.expect(TokenType::RParen)?;
        Ok(tup)
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
/// 2 MiB the native-stack envelope in docs/limits.md promises, set
/// explicitly so `RUST_MIN_STACK` cannot loosen it. Instrumented `hotpath`
/// builds wrap parser functions in timing guards and get more.
#[cfg(all(test, not(feature = "hotpath")))]
pub(crate) const DEEP_PARSE_TEST_STACK_BYTES: usize = 2 << 20;
#[cfg(all(test, feature = "hotpath"))]
pub(crate) const DEEP_PARSE_TEST_STACK_BYTES: usize = 8 << 20;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod line_attribution_tests;
