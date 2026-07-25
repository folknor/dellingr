use super::Bytecode;
use super::Instr;
use super::Result;
use super::UpvalueDesc;
use super::error::Error;
use super::error::ErrorKind;
use super::error::SyntaxError;
use super::exp_desc::CallSite;
use super::exp_desc::ExpDesc;
use super::exp_desc::PlaceExp;
use super::exp_desc::PrefixExp;
use super::lexer::TokenStream;
use super::token::Token;
use super::token::TokenType;
use crate::instr::{ArgCount, Builtin, RetCount};
use crate::numeral::is_lua_whitespace;

use std::borrow::Borrow;

pub(crate) const MAX_SYNTAX_DEPTH: u32 = 200;

mod expr;
mod func;
mod stmt;
mod table;
mod upvalue;

/// Tracks the current state, to make parsing easier.
#[derive(Debug)]
struct Parser<'a> {
    /// The input token stream.
    input: TokenStream<'a>,
    chunk: Bytecode,
    nest_level: i32,
    locals: Vec<(String, i32)>,
    outer_chunks: Vec<Bytecode>,
    /// Break-jump state for each nested loop.
    loop_breaks: Vec<LoopContext>,
    /// Upvalues for the current function being compiled.
    /// Each entry is (name, descriptor).
    upvalues: Vec<(String, UpvalueDesc)>,
    /// Stack of locals from outer functions (pushed when entering a nested function).
    outer_locals: Vec<Vec<(String, i32)>>,
    /// Stack of upvalues from outer functions.
    outer_upvalues: Vec<Vec<(String, UpvalueDesc)>>,
    /// Current line number for instruction emission.
    current_line: u32,
    /// Current nesting depth across recursive parser entry points.
    syntax_depth: u32,
}

/// Tracks the break jumps and upvalue-close boundary for one loop.
#[derive(Debug)]
struct LoopContext {
    close_slot: u8,
    break_jumps: Vec<usize>,
}

/// Parses Lua source code into a `Bytecode`.
pub(super) fn parse_str(source: &str) -> Result<Bytecode> {
    parse_str_named(source, None)
}

/// Parses Lua source code into a `Bytecode` with an optional source name.
#[hotpath::measure]
pub(super) fn parse_str_named(source: &str, source_name: Option<String>) -> Result<Bytecode> {
    let chunk = Bytecode {
        source: source_name,
        ..Default::default()
    };
    let parser = Parser {
        input: TokenStream::new(source),
        chunk,
        nest_level: 0,
        locals: Vec::new(),
        outer_chunks: Vec::new(),
        loop_breaks: Vec::new(),
        upvalues: Vec::new(),
        outer_locals: Vec::new(),
        outer_upvalues: Vec::new(),
        current_line: 1,
        syntax_depth: 0,
    };
    parser.parse_all()
}

impl<'a> Parser<'a> {
    // Helper functions

    /// Creates a new local slot at the current nest_level.
    /// Fails if we have exceeded the maximum number of locals.
    fn add_local(&mut self, name: &str) -> Result<()> {
        if self.locals.len() == u8::MAX as usize {
            Err(self.error(SyntaxError::TooManyLocals))
        } else {
            self.locals.push((name.to_string(), self.nest_level));
            let non_param_locals = self.locals.len() - self.chunk.num_params as usize;
            self.chunk.num_locals = self.chunk.num_locals.max(non_param_locals as u8);
            Ok(())
        }
    }

    /// Checks whether this scope can accommodate more locals without changing it.
    fn ensure_local_capacity(&self, additional: usize) -> Result<()> {
        if additional > u8::MAX as usize - self.locals.len() {
            Err(self.error(SyntaxError::TooManyLocals))
        } else {
            Ok(())
        }
    }

    /// Constructs an error of the given kind at the current position.
    // TODO: rename to error_here
    #[must_use]
    fn error(&self, kind: impl Into<ErrorKind>) -> Error {
        let pos = self.input.pos();
        self.error_at(kind, pos)
    }

    /// Constructs an error of the given kind and position.
    #[must_use]
    fn error_at(&self, kind: impl Into<ErrorKind>, pos: usize) -> Error {
        let (line, column) = self.input.line_and_column(pos);
        Error::new(kind, line, column)
    }

    fn checked_fixed_arg_count(&self, explicit: usize, implicit: usize) -> Result<u8> {
        let total = explicit + implicit;
        if total > 254 {
            return Err(self.error(SyntaxError::TooManyArguments));
        }
        Ok(total as u8)
    }

    fn enter_syntax_level(&mut self) -> Result<()> {
        if self.syntax_depth >= MAX_SYNTAX_DEPTH {
            return Err(self.error(SyntaxError::TooManySyntaxLevels));
        }
        self.syntax_depth += 1;
        Ok(())
    }

    fn exit_syntax_level(&mut self) {
        self.syntax_depth -= 1;
    }

    fn checked_jump_offset(&self, from: usize, target: usize) -> Result<i16> {
        let delta = target as isize - (from as isize + 1);
        i16::try_from(delta).map_err(|_| self.error(SyntaxError::JumpTooFar))
    }

    fn patch_jump(
        &mut self,
        from: usize,
        target: usize,
        constructor: impl FnOnce(i16) -> Instr,
    ) -> Result<()> {
        self.chunk.code[from] = constructor(self.checked_jump_offset(from, target)?);
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
            SyntaxError::UnexpectedTok(format!("'{exp}' expected near {got}"))
        };
        self.error_at(error_kind, token.start)
    }

    /// Returns a human-readable description of the given token.
    fn describe_token(&self, token: &Token) -> String {
        match token.typ {
            TokenType::Identifier
            | TokenType::LiteralNumber
            | TokenType::LiteralHexNumber
            | TokenType::LiteralString => {
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
        match typ {
            TokenType::And => "and",
            TokenType::Break => "break",
            TokenType::Do => "do",
            TokenType::Else => "else",
            TokenType::ElseIf => "elseif",
            TokenType::End => "end",
            TokenType::False => "false",
            TokenType::For => "for",
            TokenType::Function => "function",
            TokenType::If => "if",
            TokenType::In => "in",
            TokenType::Local => "local",
            TokenType::Nil => "nil",
            TokenType::Not => "not",
            TokenType::Or => "or",
            TokenType::Repeat => "repeat",
            TokenType::Return => "return",
            TokenType::Then => "then",
            TokenType::True => "true",
            TokenType::Until => "until",
            TokenType::While => "while",
            TokenType::Plus => "+",
            TokenType::Minus => "-",
            TokenType::Star => "*",
            TokenType::Slash => "/",
            TokenType::Mod => "%",
            TokenType::Caret => "^",
            TokenType::Hash => "#",
            TokenType::Equal => "==",
            TokenType::NotEqual => "~=",
            TokenType::LessEqual => "<=",
            TokenType::GreaterEqual => ">=",
            TokenType::Less => "<",
            TokenType::Greater => ">",
            TokenType::LParen | TokenType::LParenLineStart => "(",
            TokenType::RParen => ")",
            TokenType::LCurly => "{",
            TokenType::RCurly => "}",
            TokenType::LSquare => "[",
            TokenType::RSquare => "]",
            TokenType::Semi => ";",
            TokenType::Colon => ":",
            TokenType::Comma => ",",
            TokenType::Dot => ".",
            TokenType::DotDot => "..",
            TokenType::DotDotDot => "...",
            TokenType::Assign => "=",
            TokenType::Identifier => "<name>",
            TokenType::LiteralNumber | TokenType::LiteralHexNumber => "<number>",
            TokenType::LiteralString => "<string>",
            TokenType::EndOfFile => "<eof>",
        }
    }

    /// Pulls a token off the input and checks it against `expected`.
    /// Returns the token if it matches, `Err` otherwise.
    fn expect(&mut self, expected: TokenType) -> Result<Token> {
        let token = self.input.next()?;
        self.update_line(token.start);
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
        match self
            .chunk
            .string_literals
            .iter()
            .position(|existing| existing.as_slice() == bytes)
        {
            Some(i) => Ok(i as u16),
            None => {
                let i = self.chunk.string_literals.len();
                if i > u16::MAX as usize {
                    Err(self.error(SyntaxError::TooManyStrings))
                } else {
                    self.chunk.string_literals.push(bytes.to_vec());
                    Ok(i as u16)
                }
            }
        }
    }

    /// Stores a literal number and returns its index.
    fn find_or_add_number(&mut self, num: f64) -> Result<u16> {
        find_or_add(&mut self.chunk.number_literals, &num)
            .ok_or_else(|| self.error(SyntaxError::TooManyNumbers))
    }

    /// Converts a literal string's offsets into Lua string bytes, processing escape sequences.
    fn get_literal_string_contents(&self, tok: Token) -> Result<Vec<u8>> {
        // Chop off the quotes
        let Token { start, len, typ } = tok;
        assert_eq!(typ, TokenType::LiteralString);
        assert!(len >= 2);
        let range = (start + 1)..(start + len as usize - 1);
        let raw = self.input.substring(range).as_bytes();

        // Process escape sequences
        let mut result = Vec::with_capacity(raw.len());
        let mut raw_offset = 0;
        while raw_offset < raw.len() {
            let byte = raw[raw_offset];
            raw_offset += 1;
            if byte == b'\\' {
                let error_pos = start + 1 + raw_offset - 1;
                let next = *raw
                    .get(raw_offset)
                    .ok_or_else(|| self.error_at(SyntaxError::InvalidEscapeSequence, error_pos))?;
                raw_offset += 1;
                match next {
                    b'0'..=b'9' => {
                        let mut value = u16::from(next - b'0');
                        let mut digits = 1;
                        while digits < 3 {
                            let Some(&digit) = raw.get(raw_offset) else {
                                break;
                            };
                            if !digit.is_ascii_digit() {
                                break;
                            }
                            value = value * 10 + u16::from(digit - b'0');
                            raw_offset += 1;
                            digits += 1;
                        }
                        if value > 255 {
                            return Err(
                                self.error_at(SyntaxError::DecimalEscapeTooLarge, error_pos)
                            );
                        }
                        result.push(value as u8);
                    }
                    b'x' => {
                        let high = *raw.get(raw_offset).ok_or_else(|| {
                            self.error_at(SyntaxError::HexadecimalDigitExpected, error_pos)
                        })?;
                        let low = *raw.get(raw_offset + 1).ok_or_else(|| {
                            self.error_at(SyntaxError::HexadecimalDigitExpected, error_pos)
                        })?;
                        let high = hex_value(high).ok_or_else(|| {
                            self.error_at(SyntaxError::HexadecimalDigitExpected, error_pos)
                        })?;
                        let low = hex_value(low).ok_or_else(|| {
                            self.error_at(SyntaxError::HexadecimalDigitExpected, error_pos)
                        })?;
                        result.push(high * 16 + low);
                        raw_offset += 2;
                    }
                    b'z' => {
                        while raw
                            .get(raw_offset)
                            .is_some_and(|byte| is_lua_whitespace(*byte))
                        {
                            raw_offset += 1;
                        }
                    }
                    b'n' => result.push(b'\n'),
                    newline @ (b'\r' | b'\n') => {
                        result.push(b'\n');
                        if raw
                            .get(raw_offset)
                            .is_some_and(|next| *next != newline && matches!(*next, b'\r' | b'\n'))
                        {
                            raw_offset += 1;
                        }
                    }
                    b't' => result.push(b'\t'),
                    b'r' => result.push(b'\r'),
                    b'\\' => result.push(b'\\'),
                    b'"' => result.push(b'"'),
                    b'\'' => result.push(b'\''),
                    b'a' => result.push(b'\x07'), // bell
                    b'b' => result.push(b'\x08'), // backspace
                    b'f' => result.push(b'\x0C'), // form feed
                    b'v' => result.push(b'\x0B'), // vertical tab
                    _ => return Err(self.error_at(SyntaxError::InvalidEscapeSequence, error_pos)),
                }
            } else {
                result.push(byte);
            }
        }
        Ok(result)
    }

    /// Gets the original source code contained by a token.
    #[must_use]
    fn get_text(&self, token: Token) -> &'a str {
        self.input.substring(token.range())
    }

    /// Lowers the nesting level by one, closing and discarding its locals.
    fn level_down(&mut self) {
        let before = self.locals.len();
        while let Some((_, lvl)) = self.locals.last() {
            if *lvl == self.nest_level {
                self.locals.pop();
            } else {
                break;
            }
        }
        let block_base = self.locals.len();
        if block_base < before {
            self.push(Instr::close_upvalues(block_base as u8));
        }
        self.nest_level -= 1;
    }

    /// Adds an instruction to the output with line number tracking.
    fn push(&mut self, instr: Instr) {
        self.chunk.code.push(instr);
        self.chunk.line_info.push(self.current_line);
    }

    /// Adds an instruction with an explicit source line.
    fn push_at_line(&mut self, instr: Instr, line: u32) {
        self.chunk.code.push(instr);
        self.chunk.line_info.push(line);
    }

    /// Removes the instruction at `idx`, keeping line_info aligned.
    fn remove_instr(&mut self, idx: usize) -> Instr {
        self.chunk.line_info.remove(idx);
        self.chunk.code.remove(idx)
    }

    /// Overwrites the last emitted instruction in place, returning the old one.
    fn replace_last_instr(&mut self, instr: Instr) -> Instr {
        let slot = self
            .chunk
            .code
            .last_mut()
            .expect("replace_last_instr requires a previously emitted instruction");
        std::mem::replace(slot, instr)
    }

    /// Updates current line based on token position.
    fn update_line(&mut self, pos: usize) {
        let (line, _) = self.input.line_and_column(pos);
        self.current_line = line as u32;
    }

    /// Called when entering a loop to track break statements.
    fn enter_loop(&mut self, close_slot: u8) {
        self.loop_breaks.push(LoopContext {
            close_slot,
            break_jumps: Vec::new(),
        });
    }

    /// Called when exiting a loop. Patches all break jumps to jump to the
    /// current instruction position (the instruction after the loop).
    fn exit_loop(&mut self) -> Result<()> {
        let context = self
            .loop_breaks
            .pop()
            .expect("exit_loop called without enter_loop");
        let loop_end = self.chunk.code.len();
        for break_idx in context.break_jumps {
            // The break instruction is a Jump with placeholder offset.
            // Patch it to jump to the end of the loop.
            self.patch_jump(break_idx, loop_end, Instr::jump)?;
        }
        Ok(())
    }

    /// Records a break statement. Returns an error if not inside a loop.
    fn add_break(&mut self) -> Result<()> {
        if let Some(close_slot) = self.loop_breaks.last().map(|context| context.close_slot) {
            self.push(Instr::close_upvalues(close_slot));
            // Record the index where we'll emit the Jump instruction.
            let idx = self.chunk.code.len();
            self.loop_breaks
                .last_mut()
                .expect("loop context existed before emitting break close")
                .break_jumps
                .push(idx);
            // Emit a placeholder Jump that will be patched by exit_loop
            self.push(Instr::jump(0));
            Ok(())
        } else {
            Err(self.error(SyntaxError::BreakOutsideLoop))
        }
    }

    // Actual parsing

    /// The main entry point for the parser. This parses the entire input.
    #[hotpath::measure]
    fn parse_all(mut self) -> Result<Bytecode> {
        // The top-level chunk is a vararg function (can receive command-line args)
        let c = self.parse_chunk(&[], true)?;
        let token = self.input.next()?;
        assert_eq!(self.nest_level, 0);
        if let TokenType::EndOfFile = token.typ {
            Ok(c)
        } else {
            Err(self.err_unexpected(token, TokenType::EndOfFile))
        }
    }

    /// Parses a `Bytecode`.
    #[hotpath::measure]
    fn parse_chunk(&mut self, params: &[&str], is_vararg: bool) -> Result<Bytecode> {
        let num_params =
            u8::try_from(params.len()).map_err(|_| self.error(SyntaxError::TooManyLocals))?;
        let source = self.chunk.source.clone();
        self.outer_chunks.push(self.chunk.clone());
        self.chunk = Bytecode::default();
        self.chunk.source = source;
        self.chunk.is_vararg = is_vararg;

        // Save and reset locals for the new chunk - each function has its own
        // local variable slots starting at 0
        let saved_locals = std::mem::take(&mut self.locals);
        self.outer_locals.push(saved_locals);

        // Save and reset upvalues for the new chunk
        let saved_upvalues = std::mem::take(&mut self.upvalues);
        self.outer_upvalues.push(saved_upvalues);

        self.chunk.num_params = num_params;
        for &param in params {
            self.locals.push((param.into(), self.nest_level));
        }

        self.parse_statements()?;
        self.push(Instr::ret(RetCount::Fixed(0)));

        // Copy upvalues to chunk
        self.chunk.upvalues = self.upvalues.iter().map(|(_, desc)| *desc).collect();

        let tmp_chunk = self.chunk.clone();
        self.chunk = self.outer_chunks.pop().ok_or_else(|| {
            self.error_at(
                ErrorKind::InternalError("compiler: outer chunk stack empty".into()),
                0,
            )
        })?;

        // Restore outer locals and upvalues
        self.locals = self.outer_locals.pop().ok_or_else(|| {
            self.error_at(
                ErrorKind::InternalError("compiler: outer locals stack empty".into()),
                0,
            )
        })?;
        self.upvalues = self.outer_upvalues.pop().ok_or_else(|| {
            self.error_at(
                ErrorKind::InternalError("compiler: outer upvalues stack empty".into()),
                0,
            )
        })?;

        #[cfg(feature = "debug_parser")]
        println!("Compiled chunk: {tmp_chunk:#?}");

        Ok(tmp_chunk)
    }

    /// Parses 0 or more statements, possibly separated by semicolons.
    #[hotpath::measure]
    fn parse_statements(&mut self) -> Result<()> {
        self.enter_syntax_level()?;
        let result = self.parse_statements_inner();
        self.exit_syntax_level();
        result
    }

    fn parse_statements_inner(&mut self) -> Result<()> {
        loop {
            // Update line number at start of each statement for accurate error reporting
            let stmt_start = self.input.peek()?.start;
            self.update_line(stmt_start);

            match self.input.peek_type()? {
                TokenType::Identifier | TokenType::LParen | TokenType::LParenLineStart => {
                    self.parse_assign_or_call()?;
                }
                TokenType::If => self.parse_if()?,
                TokenType::While => self.parse_while()?,
                TokenType::Repeat => self.parse_repeat()?,
                TokenType::Do => self.parse_do()?,
                TokenType::Local => self.parse_locals()?,
                TokenType::For => self.parse_for()?,
                TokenType::Function => self.parse_fndecl()?,
                TokenType::Semi => {
                    self.input.next()?;
                }
                TokenType::Return => break self.parse_return(),
                TokenType::Break => {
                    self.input.next()?; // consume 'break' keyword
                    self.add_break()?;
                }
                _ => break Ok(()),
            }
        }
    }

    /// Emits code to evaluate the prefix expression as a normal expression.
    fn eval_prefix_exp(&mut self, exp: &PrefixExp) {
        match exp {
            PrefixExp::FunctionCall(call) => {
                let (num_args, line) = (call.num_args(), call.line());
                self.push_at_line(
                    Instr::call(ArgCount::Fixed(num_args), RetCount::Fixed(1)),
                    line,
                );
            }
            PrefixExp::Parenthesized => (),
            PrefixExp::Place(place) => {
                let instr = match place {
                    PlaceExp::Local(i) => Instr::get_local(*i),
                    PlaceExp::Upvalue(i) => Instr::get_upvalue(*i),
                    PlaceExp::Global(i) => Instr::get_global(*i),
                    PlaceExp::Builtin(b) => Instr::get_builtin(*b),
                    PlaceExp::FieldAccess(i) => Instr::get_field(*i),
                    PlaceExp::TableIndex => Instr::get_table(),
                };
                self.push(instr);
            }
        }
    }

    /// Parses a variable's name. Returns Local, Upvalue, or Global.
    fn parse_prefix_identifier(&mut self, name: &str) -> Result<PlaceExp> {
        // First check if it's a local in the current function
        if let Some(i) = find_last_local(&self.locals, name) {
            return Ok(PlaceExp::Local(i as u8));
        }

        // Check if it's already an upvalue
        if let Some(i) = self.find_upvalue(name) {
            return Ok(PlaceExp::Upvalue(i));
        }

        // Try to resolve as an upvalue from outer scopes
        if let Some(i) = self.resolve_upvalue(name)? {
            return Ok(PlaceExp::Upvalue(i));
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
fn find_last_local(locals: &[(String, i32)], name: &str) -> Option<usize> {
    let mut i = locals.len();
    while i > 0 {
        i -= 1;
        if locals[i].0 == name {
            return Some(i);
        }
    }

    None
}

/// Converts one ASCII hexadecimal digit into its value.
#[must_use]
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Returns the index of an entry in the literals list, adding it if it does not exist.
fn find_or_add<T, E>(queue: &mut Vec<T>, x: &E) -> Option<u16>
where
    T: Borrow<E> + PartialEq<E>,
    E: PartialEq<T> + ToOwned<Owned = T> + ?Sized,
{
    match queue.iter().position(|y| y == x) {
        Some(i) => Some(i as u16),
        None => {
            let i = queue.len();
            if i > u16::MAX as usize {
                None
            } else {
                queue.push(x.to_owned());
                Some(i as u16)
            }
        }
    }
}

#[cfg(test)]
mod tests;
