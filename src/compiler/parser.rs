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
use super::lexer::TokenStream;
use super::token::Token;
use super::token::TokenType;
use crate::instr::{ArgCount, Builtin, RetCount};

use std::borrow::Borrow;
use std::cmp::Ordering;

mod expr;
mod upvalue;

type TableTemplateField = (u8, usize);
type ParsedTableEntry = (u8, bool, Option<TableTemplateField>);

/// Tracks the current state, to make parsing easier.
#[derive(Debug)]
struct Parser<'a> {
    /// The input token stream.
    input: TokenStream<'a>,
    chunk: Bytecode,
    nest_level: i32,
    locals: Vec<(String, i32)>,
    outer_chunks: Vec<Bytecode>,
    /// Stack of break jump indices for each nested loop.
    /// Each entry is a list of instruction indices that need patching.
    loop_breaks: Vec<Vec<usize>>,
    /// Upvalues for the current function being compiled.
    /// Each entry is (name, descriptor).
    upvalues: Vec<(String, UpvalueDesc)>,
    /// Stack of locals from outer functions (pushed when entering a nested function).
    outer_locals: Vec<Vec<(String, i32)>>,
    /// Stack of upvalues from outer functions.
    outer_upvalues: Vec<Vec<(String, UpvalueDesc)>>,
    /// Current line number for instruction emission.
    current_line: u32,
}

#[derive(Debug)]
struct TableTemplateCandidate {
    pure_named: bool,
    inline: [TableTemplateField; 4],
    inline_len: u8,
    fields: Option<Vec<TableTemplateField>>,
}

impl TableTemplateCandidate {
    fn new() -> Self {
        Self {
            pure_named: true,
            inline: [(0, 0); 4],
            inline_len: 0,
            fields: None,
        }
    }

    fn push(&mut self, field: Option<TableTemplateField>) {
        if !self.pure_named {
            return;
        }

        let Some(field) = field else {
            self.pure_named = false;
            self.fields = None;
            return;
        };

        if let Some(fields) = self.fields.as_mut() {
            fields.push(field);
        } else if (self.inline_len as usize) < self.inline.len() {
            self.inline[self.inline_len as usize] = field;
            self.inline_len += 1;
        } else {
            let mut fields = Vec::with_capacity(self.inline.len() + 1);
            fields.extend_from_slice(&self.inline);
            fields.push(field);
            self.fields = Some(fields);
        }
    }

    fn fields_for_template(&self, field_count: u8) -> Option<&[TableTemplateField]> {
        if self.pure_named && field_count > self.inline.len() as u8 {
            self.fields.as_deref()
        } else {
            None
        }
    }
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
            if self.locals.len() > self.chunk.num_locals as usize {
                self.chunk.num_locals += 1;
            }
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
    fn expect_identifier_id(&mut self) -> Result<u8> {
        let name = self.expect_identifier()?;
        self.find_or_add_string(name)
    }

    /// Stores a literal string and returns its index.
    fn find_or_add_string(&mut self, string: &str) -> Result<u8> {
        self.find_or_add_string_bytes(string.as_bytes())
    }

    /// Stores literal string bytes and returns its index.
    fn find_or_add_string_bytes(&mut self, bytes: &[u8]) -> Result<u8> {
        match self
            .chunk
            .string_literals
            .iter()
            .position(|existing| existing.as_slice() == bytes)
        {
            Some(i) => Ok(i as u8),
            None => {
                let i = self.chunk.string_literals.len();
                if i == u8::MAX as usize {
                    Err(self.error(SyntaxError::TooManyStrings))
                } else {
                    self.chunk.string_literals.push(bytes.to_vec());
                    Ok(i as u8)
                }
            }
        }
    }

    /// Stores a literal number and returns its index.
    fn find_or_add_number(&mut self, num: f64) -> Result<u8> {
        find_or_add(&mut self.chunk.number_literals, &num)
            .ok_or_else(|| self.error(SyntaxError::TooManyNumbers))
    }

    /// Converts a literal string's offsets into Lua string bytes, processing escape sequences.
    #[must_use]
    fn get_literal_string_contents(&self, tok: Token) -> Vec<u8> {
        // Chop off the quotes
        let Token { start, len, typ } = tok;
        assert_eq!(typ, TokenType::LiteralString);
        assert!(len >= 2);
        let range = (start + 1)..(start + len as usize - 1);
        let raw = self.input.substring(range);

        // Process escape sequences
        let mut result = Vec::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    match next {
                        'n' | '\n' => result.push(b'\n'), // \n escape or literal escaped newline
                        't' => result.push(b'\t'),
                        'r' => result.push(b'\r'),
                        '\\' => result.push(b'\\'),
                        '"' => result.push(b'"'),
                        '\'' => result.push(b'\''),
                        '0' => result.push(b'\0'),
                        'a' => result.push(b'\x07'), // bell
                        'b' => result.push(b'\x08'), // backspace
                        'f' => result.push(b'\x0C'), // form feed
                        'v' => result.push(b'\x0B'), // vertical tab
                        _ => {
                            // Unknown escape, keep as-is
                            result.push(b'\\');
                            push_char_bytes(&mut result, next);
                        }
                    }
                } else {
                    result.push(b'\\');
                }
            } else {
                push_char_bytes(&mut result, c);
            }
        }
        result
    }

    /// Gets the original source code contained by a token.
    #[must_use]
    fn get_text(&self, token: Token) -> &'a str {
        self.input.substring(token.range())
    }

    /// Lowers the nesting level by one, discarding any locals from that block.
    fn level_down(&mut self) {
        while let Some((_, lvl)) = self.locals.last() {
            if *lvl == self.nest_level {
                self.locals.pop();
            } else {
                break;
            }
        }
        self.nest_level -= 1;
    }

    /// Adds an instruction to the output with line number tracking.
    fn push(&mut self, instr: Instr) {
        self.chunk.code.push(instr);
        self.chunk.line_info.push(self.current_line);
    }

    /// Updates current line based on token position.
    fn update_line(&mut self, pos: usize) {
        let (line, _) = self.input.line_and_column(pos);
        self.current_line = line as u32;
    }

    /// Called when entering a loop to track break statements.
    fn enter_loop(&mut self) {
        self.loop_breaks.push(Vec::new());
    }

    /// Called when exiting a loop. Patches all break jumps to jump to the
    /// current instruction position (the instruction after the loop).
    fn exit_loop(&mut self) {
        let breaks = self
            .loop_breaks
            .pop()
            .expect("exit_loop called without enter_loop");
        let loop_end = self.chunk.code.len();
        for break_idx in breaks {
            // The break instruction is a Jump with placeholder offset.
            // Patch it to jump to the end of the loop.
            let offset = (loop_end - break_idx - 1) as i16;
            self.chunk.code[break_idx] = Instr::jump(offset);
        }
    }

    /// Records a break statement. Returns an error if not inside a loop.
    fn add_break(&mut self) -> Result<()> {
        if let Some(breaks) = self.loop_breaks.last_mut() {
            // Record the index where we'll emit the Jump instruction
            let idx = self.chunk.code.len();
            breaks.push(idx);
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

        self.chunk.num_params = params.len() as u8;
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
                    break Ok(());
                }
                _ => break Ok(()),
            }
        }
    }

    /// Parses a function declaration, which is any statement that starts with
    /// the keyword `function`.
    fn parse_fndecl(&mut self) -> Result<()> {
        self.input.next()?; // 'function' keyword
        let name = self.expect_identifier()?;
        match self.input.peek_type()? {
            TokenType::Dot => self.parse_fndecl_table(name),
            TokenType::Colon => self.parse_fndecl_method(name),
            _ => self.parse_fndecl_basic(name),
        }
    }

    /// Parses a basic function declaration, which just assigns the function to
    /// a local, upvalue, or global variable.
    fn parse_fndecl_basic(&mut self, name: &'a str) -> Result<()> {
        let place_exp = self.parse_prefix_identifier(name)?;
        let instr = match place_exp {
            PlaceExp::Local(i) => Instr::set_local(i),
            PlaceExp::Upvalue(i) => Instr::set_upvalue(i),
            PlaceExp::Global(i) => Instr::set_global(i),
            PlaceExp::Builtin(b) => Instr::set_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.parse_fndef_named(Some(name.to_string()))?;
        self.push(instr);
        Ok(())
    }

    fn parse_fndecl_table(&mut self, table_name: &'a str) -> Result<()> {
        // Push the table onto the stack.
        let table_instr = match self.parse_prefix_identifier(table_name)? {
            PlaceExp::Local(i) => Instr::get_local(i),
            PlaceExp::Upvalue(i) => Instr::get_upvalue(i),
            PlaceExp::Global(i) => Instr::get_global(i),
            PlaceExp::Builtin(b) => Instr::get_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.push(table_instr);

        // Parse all the fields, building the full name (e.g., "foo.bar.baz").
        let mut full_name = table_name.to_string();
        self.expect(TokenType::Dot)?;
        let mut last_field = self.expect_identifier()?;
        full_name.push('.');
        full_name.push_str(last_field);
        let mut last_field_id = self.find_or_add_string(last_field)?;

        while self.input.try_pop(TokenType::Dot)?.is_some() {
            self.push(Instr::get_field(last_field_id));
            last_field = self.expect_identifier()?;
            full_name.push('.');
            full_name.push_str(last_field);
            last_field_id = self.find_or_add_string(last_field)?;
        }

        if self.input.try_pop(TokenType::Colon)?.is_some() {
            self.push(Instr::get_field(last_field_id));
            let method_name = self.expect_identifier()?;
            full_name.push(':');
            full_name.push_str(method_name);
            let method_name_id = self.find_or_add_string(method_name)?;

            self.parse_fndef_method(Some(full_name))?;
            self.push(Instr::set_field(0, method_name_id));
        } else {
            self.parse_fndef_named(Some(full_name))?;
            self.push(Instr::set_field(0, last_field_id));
        }
        Ok(())
    }

    /// Parses a method declaration: `function table:method()`
    /// This is sugar for `table.method = function(self, ...)`
    fn parse_fndecl_method(&mut self, table_name: &'a str) -> Result<()> {
        // Push the table onto the stack.
        let table_instr = match self.parse_prefix_identifier(table_name)? {
            PlaceExp::Local(i) => Instr::get_local(i),
            PlaceExp::Upvalue(i) => Instr::get_upvalue(i),
            PlaceExp::Global(i) => Instr::get_global(i),
            PlaceExp::Builtin(b) => Instr::get_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.push(table_instr);

        // Consume the colon and get the method name.
        self.expect(TokenType::Colon)?;
        let method_name = self.expect_identifier()?;
        let full_name = format!("{table_name}:{method_name}");
        let method_name_id = self.find_or_add_string(method_name)?;

        // Parse the function params and body with implicit self.
        self.parse_fndef_method(Some(full_name))?;
        self.push(Instr::set_field(0, method_name_id));
        Ok(())
    }

    /// Parses a return statement. Return statements must always come last in a
    /// block.
    fn parse_return(&mut self) -> Result<()> {
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
    fn parse_assign_or_call(&mut self) -> Result<()> {
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

    /// Emits code to evaluate the prefix expression as a normal expression.
    fn eval_prefix_exp(&mut self, exp: &PrefixExp) {
        match exp {
            PrefixExp::FunctionCall(num_args) => {
                self.push(Instr::call(ArgCount::Fixed(*num_args), RetCount::Fixed(1)));
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
        if let Some(i) = self.resolve_upvalue(name) {
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

    /// Parses a `local` declaration.
    fn parse_locals(&mut self) -> Result<()> {
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
    fn parse_for(&mut self) -> Result<()> {
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
    fn parse_do(&mut self) -> Result<()> {
        self.input.next()?; // `do` keyword
        self.nest_level += 1;
        self.parse_statements()?;
        self.expect(TokenType::End)?;
        self.level_down();
        Ok(())
    }

    /// Parses a `repeat ... until` statement.
    fn parse_repeat(&mut self) -> Result<()> {
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
    fn parse_while(&mut self) -> Result<()> {
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
    fn parse_if(&mut self) -> Result<()> {
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

    /// Parses the parameters in a function definition.
    /// Returns (params, is_vararg).
    fn parse_params(&mut self) -> Result<(Vec<&'a str>, bool)> {
        let lparen_tok = self.input.next()?;
        match lparen_tok.typ {
            TokenType::LParen | TokenType::LParenLineStart => (),
            _ => return Err(self.err_unexpected(lparen_tok, TokenType::LParen)),
        }
        let mut args = Vec::new();
        let mut is_vararg = false;

        if self.input.try_pop(TokenType::RParen)?.is_some() {
            return Ok((args, is_vararg));
        }

        // Check for vararg-only function: function(...)
        if self.input.try_pop(TokenType::DotDotDot)?.is_some() {
            is_vararg = true;
            self.expect(TokenType::RParen)?;
            return Ok((args, is_vararg));
        }

        args.push(self.expect_identifier()?);
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            // Check for vararg after comma: function(a, b, ...)
            if self.input.try_pop(TokenType::DotDotDot)?.is_some() {
                is_vararg = true;
                break;
            }
            args.push(self.expect_identifier()?);
        }
        self.expect(TokenType::RParen)?;
        Ok((args, is_vararg))
    }

    /// Parses the parameters and body of a function definition.
    fn parse_fndef(&mut self) -> Result<()> {
        self.parse_fndef_named(None)
    }

    /// Parses the parameters and body of a function definition with an optional name.
    fn parse_fndef_named(&mut self, name: Option<String>) -> Result<()> {
        let (params, is_vararg) = self.parse_params()?;
        if self.chunk.nested.len() >= u8::MAX as usize {
            return Err(self.error(SyntaxError::TooManyNestedFunctions));
        }

        self.nest_level += 1;
        let mut new_chunk = self.parse_chunk(&params, is_vararg)?;
        new_chunk.name = name;
        self.level_down();

        self.chunk.nested.push(Arc::new(new_chunk));
        self.push(Instr::closure(self.chunk.nested.len() as u8 - 1));
        self.expect(TokenType::End)?;
        Ok(())
    }

    /// Parses a method definition with implicit `self` parameter.
    /// Used for `function table:method()` syntax.
    fn parse_fndef_method(&mut self, name: Option<String>) -> Result<()> {
        let (mut params, is_vararg) = self.parse_params()?;
        // Prepend "self" to the parameter list
        params.insert(0, "self");
        if self.chunk.nested.len() >= u8::MAX as usize {
            return Err(self.error(SyntaxError::TooManyNestedFunctions));
        }

        self.nest_level += 1;
        let mut new_chunk = self.parse_chunk(&params, is_vararg)?;
        new_chunk.name = name;
        self.level_down();

        self.chunk.nested.push(Arc::new(new_chunk));
        self.push(Instr::closure(self.chunk.nested.len() as u8 - 1));
        self.expect(TokenType::End)?;
        Ok(())
    }

    /// Parses a table constructor.
    #[hotpath::measure]
    fn parse_table(&mut self) -> Result<()> {
        let table_instr_idx = self.chunk.code.len();
        self.push(Instr::new_table());
        // Skip any leading separators (handles {;} and {,})
        while let TokenType::Comma | TokenType::Semi = self.input.peek_type()? {
            self.input.next()?;
        }
        if self.input.try_pop(TokenType::RCurly)?.is_none() {
            // i is the number of array-style entries.
            let mut i = 0;
            let mut field_count = 0u8;
            let mut has_vararg = false;
            let mut template_candidate = TableTemplateCandidate::new();
            let (new_i, is_vararg, named_field) = self.parse_table_entry(i)?;
            i = new_i;
            field_count = field_count.saturating_add(1);
            has_vararg = has_vararg || is_vararg;
            template_candidate.push(named_field);
            while let TokenType::Comma | TokenType::Semi = self.input.peek_type()? {
                self.input.next()?;
                if self.input.check_type(TokenType::RCurly)? {
                    break;
                }
                let (new_i, is_vararg, named_field) = self.parse_table_entry(i)?;
                i = new_i;
                field_count = field_count.saturating_add(1);
                has_vararg = has_vararg || is_vararg;
                template_candidate.push(named_field);
            }
            self.expect(TokenType::RCurly)?;

            if field_count > 4
                && !template_candidate
                    .fields_for_template(field_count)
                    .is_some_and(|fields| self.try_use_table_template(table_instr_idx, fields))
            {
                self.chunk.code[table_instr_idx] = Instr::new_table_presized(field_count);
            }

            if has_vararg {
                // Use SetList(0) to indicate "use all values above table"
                self.push(Instr::set_list(0));
            } else if i > 0 {
                self.push(Instr::set_list(i));
            }
        }
        Ok(())
    }

    /// Parses a table entry.
    /// Returns (new_counter, is_vararg) where is_vararg indicates if this entry was `...`
    #[hotpath::measure]
    fn parse_table_entry(&mut self, counter: u8) -> Result<ParsedTableEntry> {
        match self.input.peek_type()? {
            TokenType::Identifier if self.input.peek2_type()? == TokenType::Assign => {
                let index = self.expect_identifier_id()?;
                self.expect(TokenType::Assign)?;
                self.parse_expr()?;
                let instr_idx = self.chunk.code.len();
                self.push(Instr::init_field(counter, index));
                Ok((counter, false, Some((index, instr_idx))))
            }
            TokenType::LSquare => {
                self.input.next()?;
                self.parse_expr()?;
                self.expect(TokenType::RSquare)?;
                self.expect(TokenType::Assign)?;
                self.parse_expr()?;
                self.push(Instr::init_index(counter));
                Ok((counter, false, None))
            }
            TokenType::DotDotDot => {
                // {...} syntax - collect all varargs into the table
                self.input.next()?;
                if !self.chunk.is_vararg {
                    return Err(self.error(SyntaxError::UnexpectedTok(
                        "cannot use '...' outside a vararg function".to_string(),
                    )));
                }
                // Push all varargs onto stack
                self.push(Instr::vararg(u8::MAX));
                // Return special marker indicating vararg was used
                Ok((counter, true, None))
            }
            _ => {
                if counter == u8::MAX {
                    return Err(self.error(SyntaxError::TooManyTableFields));
                }
                self.parse_expr()?;
                Ok((counter + 1, false, None))
            }
        }
    }

    fn try_use_table_template(
        &mut self,
        table_instr_idx: usize,
        fields: &[TableTemplateField],
    ) -> bool {
        if fields.is_empty() || self.chunk.table_templates.len() >= u8::MAX as usize {
            return false;
        }

        let mut template = Vec::with_capacity(fields.len());
        let mut field_indices = Vec::with_capacity(fields.len());
        for (key_id, _) in fields {
            if template.contains(key_id) {
                return false;
            }
            let entry_idx = match u8::try_from(template.len()) {
                Ok(entry_idx) => entry_idx,
                Err(_) => return false,
            };
            template.push(*key_id);
            field_indices.push(entry_idx);
        }

        let template_idx = self.chunk.table_templates.len() as u8;
        self.chunk.table_templates.push(template);
        self.chunk.code[table_instr_idx] = Instr::new_table_template(template_idx);
        for ((key_id, instr_idx), entry_idx) in fields.iter().zip(field_indices) {
            self.chunk.code[*instr_idx] = Instr::init_field_pinned(*key_id, entry_idx);
        }
        true
    }

    /// Parses a literal string and emits bytecode to push it.
    fn push_literal_string(&mut self, tok: Token) -> Result<()> {
        let text = self.get_literal_string_contents(tok);
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

fn push_char_bytes(out: &mut Vec<u8>, c: char) {
    let mut buf = [0; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Returns the index of an entry in the literals list, adding it if it does not exist.
fn find_or_add<T, E>(queue: &mut Vec<T>, x: &E) -> Option<u8>
where
    T: Borrow<E> + PartialEq<E>,
    E: PartialEq<T> + ToOwned<Owned = T> + ?Sized,
{
    match queue.iter().position(|y| y == x) {
        Some(i) => Some(i as u8),
        None => {
            let i = queue.len();
            if i == u8::MAX as usize {
                None
            } else {
                queue.push(x.to_owned());
                Some(i as u8)
            }
        }
    }
}

#[cfg(test)]
mod tests;
