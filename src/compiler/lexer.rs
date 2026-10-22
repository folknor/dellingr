//! This module tokenizes Lua source. Source is bytes, as Lua strings are:
//! every token the grammar knows is ASCII, so the lexer walks bytes, and the
//! only places arbitrary bytes may appear - string literals and comments -
//! are copied or skipped byte for byte. A non-ASCII byte anywhere else is a
//! syntax error, reported as the UTF-8 character it starts when it starts
//! one.

use super::Result;
use super::error::Error;
use super::error::SyntaxError;
use super::token::Token;
use super::token::TokenType::{self, *};
use crate::numeral::{hex_value, is_lua_whitespace, is_newline_pair, parse_lua_numeral};

use std::ops::Range;

/// A `TokenStream` is a wrapper around a `Lexer`. It provides a lookahead buffer and several
/// helper methods.
#[derive(Debug)]
pub(super) struct TokenStream<'a> {
    lexer: Lexer<'a>,
    lookahead: Option<Token>,
    lookahead2: Option<Token>,
    last_line: u32,
}

/// Decoded contents saved by the lexer for a short-string token.
#[derive(Debug)]
pub(super) enum LiteralStringContents {
    /// The literal decoded successfully.
    Decoded(Vec<u8>),
    /// A malformed escape, retained until the parser consumes its token.
    SyntaxError { kind: SyntaxError, pos: usize },
    /// The decoded literal exceeded the configured string limit.
    StringSizeExceeded { size: usize, limit: usize },
}

#[derive(Debug)]
enum LiteralStringFailure {
    SyntaxError(SyntaxError, usize),
    StringSizeExceeded { size: usize, limit: usize },
}

/// A `Lexer` handles the raw conversion of bytes to tokens.
#[derive(Debug)]
pub(super) struct Lexer<'a> {
    /// The position of the next byte.
    pos: usize,
    /// `linebreaks[i]` is the byte offset of the start of line `i`.
    linebreaks: Vec<usize>,
    /// The last character read was the first member of a two-character
    /// newline (`\r\n` or `\n\r`) whose line break is deferred to its second
    /// member. That second member always closes the pair, so in `\r\n\r` the
    /// trailing `\r` starts a fresh break, as reference `inclinenumber` does.
    newline_pair_open: bool,
    source: &'a [u8],
    literal_string_contents: Vec<(usize, LiteralStringContents)>,
}

impl<'a> TokenStream<'a> {
    /// Constructs a new `TokenStream`.
    #[must_use]
    pub(super) fn new(source: &'a [u8]) -> Self {
        Self {
            lexer: Lexer::new(source),
            lookahead: None,
            lookahead2: None,
            last_line: 1,
        }
    }

    /// Returns the next `Token`.
    #[hotpath::measure]
    pub(super) fn next(&mut self) -> Result<Token> {
        let token = match self.lookahead.take() {
            Some(token) => {
                self.lookahead = self.lookahead2.take();
                Ok(token)
            }
            None => self.lexer.next_token(),
        }?;
        self.last_line = token.line;
        Ok(token)
    }

    /// Line of the last consumed token. Lookahead never changes attribution.
    pub(super) fn last_line(&self) -> u32 {
        self.last_line
    }

    /// Returns the next `Token`, without popping it from the stream.
    #[hotpath::measure]
    #[allow(clippy::expect_used)] // The branch above populates this exact option.
    pub(super) fn peek(&mut self) -> Result<&Token> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.lexer.next_token()?);
        }
        Ok(self
            .lookahead
            .as_ref()
            .expect("lexer lookahead is populated before returning"))
    }

    /// Returns the token after the next token, without popping it from the stream.
    #[allow(clippy::expect_used)] // Both lookahead slots are populated before access.
    pub(super) fn peek2(&mut self) -> Result<&Token> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.lexer.next_token()?);
        }
        if self.lookahead2.is_none() {
            self.lookahead2 = Some(self.lexer.next_token()?);
        }
        Ok(self
            .lookahead2
            .as_ref()
            .expect("second lexer lookahead is populated before returning"))
    }

    /// Returns the type of the next token.
    pub(super) fn peek_type(&mut self) -> Result<TokenType> {
        Ok(self.peek()?.typ)
    }

    /// Returns the type of the token after the next token.
    pub(super) fn peek2_type(&mut self) -> Result<TokenType> {
        Ok(self.peek2()?.typ)
    }

    /// Returns whether the next token is of the given type.
    pub(super) fn check_type(&mut self, expected_type: TokenType) -> Result<bool> {
        Ok(self.peek_type()? == expected_type)
    }

    /// Checks the next token's type. If it matches `expected_type`, it is popped off and
    /// returned as `Some`. Otherwise, returns `None`.
    pub(super) fn try_pop(&mut self, expected_type: TokenType) -> Result<Option<Token>> {
        if self.check_type(expected_type)? {
            Ok(Some(self.next()?))
        } else {
            Ok(None)
        }
    }

    /// Returns the current position of the `TokenStream`.
    #[must_use]
    pub(super) fn line_and_column(&self, pos: usize) -> (u32, u32) {
        self.lexer.line_and_col(pos)
    }

    /// Returns how many bytes have been read.
    #[must_use]
    pub(super) fn pos(&self) -> usize {
        match &self.lookahead {
            Some(token) => token.start,
            None => self.lexer.pos,
        }
    }

    /// Returns a range of the source bytes; token ranges are always in bounds.
    #[must_use]
    pub(super) fn source_bytes(&self, range: Range<usize>) -> &'a [u8] {
        &self.lexer.source[range]
    }

    /// Takes decoded contents for a string token already returned by the lexer.
    pub(super) fn take_literal_string_contents(
        &mut self,
        token_start: usize,
    ) -> Option<LiteralStringContents> {
        let index = self
            .lexer
            .literal_string_contents
            .iter()
            .position(|(start, _)| *start == token_start)?;
        Some(self.lexer.literal_string_contents.remove(index).1)
    }
}

impl<'a> Lexer<'a> {
    /// Constructs a new `Lexer`.
    #[must_use]
    pub(super) fn new(source: &'a [u8]) -> Self {
        let linebreaks = vec![0];
        Self {
            linebreaks,
            newline_pair_open: false,
            pos: 0,
            source,
            literal_string_contents: Vec::new(),
        }
    }

    /// Returns the next `Token`.
    #[hotpath::measure]
    pub(super) fn next_token(&mut self) -> Result<Token> {
        // Loop rather than recurse on comments: a comment skips its body and
        // continues the loop, so a long run of consecutive comments cannot
        // exhaust the stack the way tail-recursing into `next_token` did.
        loop {
            self.consume_whitespace();
            let tok_start = self.pos;
            // Capture before consuming the body: a string token can consume
            // logical newlines through \z or an escaped physical newline.
            // The compiler source cap keeps this one-based line count and the
            // token's byte length below the u32 representational limit.
            let tok_line = self.linebreaks.len() as u32;
            let Some(first_char) = self.next_char() else {
                return Ok(self.end_of_file(tok_line));
            };
            let tok_type = match first_char {
                b'+' => Plus,
                b'*' => Star,
                b'/' => Slash,
                b'%' => Mod,
                b'^' => Caret,
                b'#' => Hash,
                b';' => Semi,
                b':' => Colon,
                b',' => Comma,
                b'(' => LParen,
                b')' => RParen,
                b'{' => LCurly,
                b'}' => RCurly,
                b']' => RSquare,

                b'.' => self.peek_dot()?,

                b'=' | b'<' | b'>' | b'~' => self.peek_equals(first_char)?,

                b'-' => {
                    if self.try_next(b'-') {
                        self.skip_comment(tok_start)?;
                        continue;
                    }
                    Minus
                }

                b'\'' | b'\"' => self.lex_string(first_char, tok_start)?,
                b'[' => {
                    if let Some(b'=' | b'[') = self.peek_char() {
                        return Err(self.error_at(SyntaxError::LongStringUnsupported, tok_start));
                    }
                    LSquare
                }

                b'0'..=b'9' => self.lex_full_number(first_char, tok_start)?,

                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_word(tok_start),

                _ => return Err(self.invalid_character(first_char, tok_start)),
            };
            let len = (self.pos - tok_start) as u32;
            return Ok(Token {
                typ: tok_type,
                start: tok_start,
                len,
                line: tok_line,
            });
        }
    }

    /// Skips over the characters in a comment body. The enclosing `next_token`
    /// loop re-scans afterward; a short comment may end at EOF, while an
    /// unfinished long comment raises an error. Iterative (no recursion) so
    /// consecutive comments are stack-safe.
    ///
    /// A long comment (`--[[` / `--[==[`) that reaches EOF before its closing
    /// delimiter is an error reported at `comment_start`, as in reference Lua
    /// ("unfinished long comment"); accepting it would silently drop the rest
    /// of the file into the comment.
    fn skip_comment(&mut self, comment_start: usize) -> Result<()> {
        // Check for a long comment: --[=*[ ... ]=*]. A failed opener probe is
        // still ordinary comment text, so falling through is correct.
        if self.peek_char() == Some(b'[') {
            self.next_char(); // consume '['
            let mut level = 0;
            while self.try_next(b'=') {
                level += 1;
            }
            if self.try_next(b'[') {
                // Long comment: skip until a closing delimiter of the same level.
                loop {
                    match self.next_char() {
                        Some(b']') => {
                            let mut closing_level = 0;
                            while self.try_next(b'=') {
                                closing_level += 1;
                            }
                            if closing_level == level && self.try_next(b']') {
                                return Ok(());
                            }
                        }
                        None => {
                            return Err(
                                self.error_at(SyntaxError::UnfinishedLongComment, comment_start)
                            );
                        }
                        _ => {}
                    }
                }
            }
            // Single '[' after '--' is just a regular comment, fall through
        }
        // Single-line comment: skip until newline (consumed) or EOF
        while let Some(c) = self.next_char() {
            if matches!(c, b'\r' | b'\n') {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Peeks the next byte.
    #[must_use]
    fn peek_char(&self) -> Option<u8> {
        self.source.get(self.pos).copied()
    }

    /// Pops and returns the next byte.
    fn next_char(&mut self) -> Option<u8> {
        let c = self.peek_char()?;
        self.pos += 1;
        // CRLF and LFCR are each one logical newline. Defer the first member,
        // then record the line start after the second. The second member never
        // opens a new pair: `\r\n\r` is two breaks.
        if matches!(c, b'\r' | b'\n') {
            if !self.newline_pair_open
                && matches!(self.peek_char(), Some(next) if is_newline_pair(c, next))
            {
                self.newline_pair_open = true;
            } else {
                self.newline_pair_open = false;
                self.linebreaks.push(self.pos);
            }
        }
        Some(c)
    }

    /// The error for a byte that starts no token. The reported position is
    /// just past the offending character: a byte that starts a valid UTF-8
    /// character consumes the whole character first, so the column matches
    /// a character-at-a-time scan. Only that one candidate character is
    /// decoded, so malformed bytes after it do not matter.
    fn invalid_character(&mut self, first: u8, start: usize) -> Error {
        if first.is_ascii() {
            return self.error(SyntaxError::InvalidCharacter(char::from(first)));
        }
        let end = self.source.len().min(start + 4);
        let decoded = self.source[start..end]
            .utf8_chunks()
            .next()
            .and_then(|chunk| chunk.valid().chars().next());
        match decoded {
            Some(c) => {
                // Continuation bytes are never newlines, so this cannot record
                // a line break.
                self.pos = start + c.len_utf8();
                self.error(SyntaxError::InvalidCharacter(c))
            }
            None => self.error(SyntaxError::InvalidByte(first)),
        }
    }

    /// Consumes any whitespace bytes.
    fn consume_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if !is_lua_whitespace(c) {
                break;
            }
            self.next_char();
        }
    }

    /// Move a byte forward, only if the current byte matches `expected`.
    fn try_next(&mut self, expected: u8) -> bool {
        match self.peek_char() {
            Some(c) if c == expected => {
                self.next_char();
                true
            }
            _ => false,
        }
    }

    /// Constructs an error of the given kind at the current position.
    #[must_use]
    fn error(&self, kind: SyntaxError) -> Error {
        self.error_at(kind, self.pos)
    }

    /// Constructs an error of the given kind at an explicit byte offset.
    #[must_use]
    fn error_at(&self, kind: SyntaxError, pos: usize) -> Error {
        let (line_num, column) = self.line_and_col(pos);
        Error::new(kind, line_num, column)
    }

    /// The lexer just read a `.`.
    /// Determines whether it is part of a `Dot`, `DotDot`, `DotDotDot` or `Number`.
    fn peek_dot(&mut self) -> Result<TokenType> {
        let typ = match self.peek_char() {
            Some(b'.') => {
                self.next_char();
                if self.try_next(b'.') {
                    DotDotDot
                } else {
                    DotDot
                }
            }
            Some(c) if c.is_ascii_digit() => {
                self.next_char();
                self.lex_number_after_decimal()?;
                LiteralNumber
            }
            _ => Dot,
        };
        Ok(typ)
    }

    /// The lexer just read something which might be part of a two-character
    /// operator, with `=` as the second character.
    ///
    /// Returns `Err` if the first character is `~` and it is not paired with a
    /// `=`.
    #[allow(clippy::panic)] // Only the four operator characters call this helper.
    fn peek_equals(&mut self, first_char: u8) -> Result<TokenType> {
        if self.try_next(b'=') {
            let typ = match first_char {
                b'=' => Equal,
                b'~' => NotEqual,
                b'<' => LessEqual,
                b'>' => GreaterEqual,
                _ => panic!("peek_equals was called with first_char = {first_char}"),
            };
            Ok(typ)
        } else {
            match first_char {
                b'=' => Ok(Assign),
                b'<' => Ok(Less),
                b'>' => Ok(Greater),
                b'~' => Err(self.error(SyntaxError::InvalidCharacter('~'))),
                _ => panic!("peek_equals was called with first_char = {first_char}"),
            }
        }
    }

    /// Tokenizes and decodes a short literal string in one pass.
    ///
    /// Escape failures are saved with the token and returned only when the
    /// parser consumes it. This preserves the parser's error ordering when a
    /// malformed string appears after an earlier syntax error. Bytes other
    /// than the quote, backslash and line breaks are copied verbatim, so a
    /// literal holds exactly the source's bytes, valid UTF-8 or not.
    fn lex_string(&mut self, quote: u8, token_start: usize) -> Result<TokenType> {
        let body_start = self.pos;
        let mut decoded = Vec::new();
        let mut failure = None;
        let mut decoded_until = body_start;

        loop {
            let char_start = self.pos;
            let Some(c) = self.next_char() else {
                return Err(self.error(SyntaxError::UnclosedString));
            };
            if c == quote {
                let contents = match failure {
                    Some(LiteralStringFailure::SyntaxError(kind, pos)) => {
                        LiteralStringContents::SyntaxError { kind, pos }
                    }
                    Some(LiteralStringFailure::StringSizeExceeded { size, limit }) => {
                        LiteralStringContents::StringSizeExceeded { size, limit }
                    }
                    None => LiteralStringContents::Decoded(decoded),
                };
                self.literal_string_contents.push((token_start, contents));
                return Ok(LiteralString);
            }

            if c == b'\\' {
                let Some(escaped) = self.next_char() else {
                    return Err(self.error(SyntaxError::UnclosedString));
                };
                if escaped == b'z' {
                    self.consume_whitespace();
                } else if matches!(escaped, b'\r' | b'\n')
                    && matches!(self.peek_char(), Some(next) if is_newline_pair(escaped, next))
                {
                    self.next_char();
                }

                if failure.is_none() {
                    match self.decode_string_escape(
                        char_start,
                        escaped,
                        quote,
                        self.pos,
                        &mut decoded,
                    ) {
                        Ok(skip_until) => decoded_until = skip_until,
                        Err(error) => failure = Some(error),
                    }
                }
            } else if matches!(c, b'\r' | b'\n') {
                return Err(self.error(SyntaxError::UnclosedString));
            } else if failure.is_none() && char_start >= decoded_until {
                match Self::check_decoded_string_growth(decoded.len()) {
                    Ok(()) => decoded.push(c),
                    Err(error) => failure = Some(error),
                }
            }
        }
    }

    fn check_decoded_string_growth(
        current_len: usize,
    ) -> std::result::Result<(), LiteralStringFailure> {
        let size = current_len.saturating_add(1);
        if size > crate::limits::MAX_STRING_BYTES {
            Err(LiteralStringFailure::StringSizeExceeded {
                size,
                limit: crate::limits::MAX_STRING_BYTES,
            })
        } else {
            Ok(())
        }
    }

    fn decode_string_escape(
        &self,
        slash_pos: usize,
        escaped: u8,
        quote: u8,
        lexical_end: usize,
        decoded: &mut Vec<u8>,
    ) -> std::result::Result<usize, LiteralStringFailure> {
        Self::check_decoded_string_growth(decoded.len())?;
        let bytes = self.source;
        let error = |kind| LiteralStringFailure::SyntaxError(kind, slash_pos);
        let unchanged_end = lexical_end;

        let skip_until = match escaped {
            b'0'..=b'9' => {
                let mut value = u16::from(escaped - b'0');
                let mut digits = 1;
                let mut end = slash_pos + 2;
                while digits < 3 {
                    let Some(&digit) = bytes.get(end) else {
                        break;
                    };
                    if !digit.is_ascii_digit() {
                        break;
                    }
                    value = value * 10 + u16::from(digit - b'0');
                    end += 1;
                    digits += 1;
                }
                if value > 255 {
                    return Err(error(SyntaxError::DecimalEscapeTooLarge));
                }
                decoded.push(value as u8);
                end
            }
            b'x' => {
                let high_pos = slash_pos + 2;
                let low_pos = high_pos + 1;
                let high = match bytes.get(high_pos) {
                    Some(&byte) if byte != quote => hex_value(byte),
                    _ => None,
                }
                .ok_or_else(|| error(SyntaxError::HexadecimalDigitExpected))?;
                let low = match bytes.get(low_pos) {
                    Some(&byte) if byte != quote => hex_value(byte),
                    _ => None,
                }
                .ok_or_else(|| error(SyntaxError::HexadecimalDigitExpected))?;
                decoded.push(high * 16 + low);
                low_pos + 1
            }
            b'z' => unchanged_end,
            b'\r' | b'\n' | b'n' => {
                decoded.push(b'\n');
                unchanged_end
            }
            b't' => {
                decoded.push(b'\t');
                unchanged_end
            }
            b'r' => {
                decoded.push(b'\r');
                unchanged_end
            }
            b'\\' => {
                decoded.push(b'\\');
                unchanged_end
            }
            b'"' => {
                decoded.push(b'"');
                unchanged_end
            }
            b'\'' => {
                decoded.push(b'\'');
                unchanged_end
            }
            b'a' => {
                decoded.push(b'\x07');
                unchanged_end
            }
            b'b' => {
                decoded.push(b'\x08');
                unchanged_end
            }
            b'f' => {
                decoded.push(b'\x0C');
                unchanged_end
            }
            b'v' => {
                decoded.push(b'\x0B');
                unchanged_end
            }
            _ => return Err(error(SyntaxError::InvalidEscapeSequence)),
        };
        Ok(skip_until)
    }

    /// Reads in a number which starts with a digit (as opposed to a decimal point).
    fn lex_full_number(&mut self, first_char: u8, tok_start: usize) -> Result<TokenType> {
        // Check for hex values (both 0x and 0X)
        if first_char == b'0' && (self.try_next(b'x') || self.try_next(b'X')) {
            // Mantissa: hex digits with an optional fraction (Lua 5.2 hex
            // floats). At least one digit must appear in the mantissa:
            // `0x1.8`, `0x1.`, and `0x.8` are valid, `0x.` is not.
            self.lex_hex_digits();
            if self.try_next(b'.') {
                self.lex_hex_digits();
            }
            // Let the shared numeral parser own mantissa validation. Check at
            // this boundary to keep malformed-mantissa diagnostics before a
            // following exponent, as in the original lexer.
            if parse_lua_numeral(&self.source[tok_start..self.pos]).is_none() {
                return Err(self.error(SyntaxError::BadNumber));
            }

            // Optional binary exponent: p/P, an optional sign, then at least
            // one DECIMAL digit (`0x1p-2`, `0x1.8p+0`).
            let has_exponent = self.try_next(b'p') || self.try_next(b'P');
            if has_exponent {
                if let Some(c) = self.peek_char()
                    && (c == b'+' || c == b'-')
                {
                    self.next_char();
                }
                self.lex_digits();
                if parse_lua_numeral(&self.source[tok_start..self.pos]).is_none() {
                    return Err(self.error(SyntaxError::BadNumber));
                }
            }

            match self.peek_char() {
                Some(c) if is_identifier_continue(c) => Err(self.error(SyntaxError::BadNumber)),
                _ => Ok(LiteralNumber),
            }
        } else {
            // Read in the rest of the base
            self.lex_digits();

            // Handle the fraction and exponent components.
            if self.try_next(b'.') {
                match self.peek_char() {
                    Some(c) if c.is_ascii_digit() => self.lex_number_after_decimal()?,
                    _ => self.lex_exponent()?,
                }
            } else {
                self.lex_exponent()?;
            }

            Ok(LiteralNumber)
        }
    }

    /// Reads in a literal number which starts with a decimal point.
    fn lex_number_after_decimal(&mut self) -> Result<()> {
        self.lex_digits();
        self.lex_exponent()
    }

    /// Consumes an unbroken sequence of digits.
    fn lex_digits(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.next_char();
            } else {
                break;
            }
        }
    }

    /// Consumes an unbroken sequence of hexadecimal digits.
    fn lex_hex_digits(&mut self) {
        while self.peek_char().is_some_and(|c| hex_value(c).is_some()) {
            self.next_char();
        }
    }

    /// Consumes the optional exponent part of a literal number, then rejects
    /// an identifier character glued to it.
    fn lex_exponent(&mut self) -> Result<()> {
        if self.try_next(b'E') || self.try_next(b'e') {
            // The exponent might have a sign.
            if let Some(c) = self.peek_char()
                && (c == b'+' || c == b'-')
            {
                self.next_char();
            }

            self.lex_digits();
        }
        match self.peek_char() {
            Some(c) if is_identifier_continue(c) => Err(self.error(SyntaxError::BadNumber)),
            _ => Ok(()),
        }
    }

    /// Reads a word and returns it as an identifier or keyword.
    fn lex_word(&mut self, tok_start: usize) -> TokenType {
        while let Some(c) = self.peek_char() {
            if is_identifier_continue(c) {
                self.next_char();
            } else {
                break;
            }
        }

        TokenType::keyword_match(&self.source[tok_start..self.pos])
    }

    /// Returns the current position of the `Lexer`.
    #[must_use]
    #[hotpath::measure]
    fn line_and_col(&self, pos: usize) -> (u32, u32) {
        // A byte at a recorded line start is column 1 of that new line;
        // newline bytes themselves precede the following recorded start.
        let line_index = self.linebreaks.partition_point(|&start| start <= pos) - 1;
        let column = pos - self.linebreaks[line_index];
        // Both values are bounded by the source length, which the parser caps
        // at `MAX_SOURCE_BYTES`; `limits` asserts that cap is below u32::MAX.
        ((line_index + 1) as u32, (column + 1) as u32)
    }

    #[must_use]
    const fn end_of_file(&self, line: u32) -> Token {
        Token {
            typ: TokenType::EndOfFile,
            start: self.pos,
            len: 0,
            line,
        }
    }
}

fn is_identifier_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str, tokens: &[(TokenType, usize, u32)], lines: &[usize]) {
        let mut lexer = Lexer::new(input.as_bytes());
        let mut tokens = tokens.iter().map(|&(typ, start, len)| Token {
            typ,
            start,
            len,
            line: lines.partition_point(|&line_start| line_start <= start) as u32,
        });
        loop {
            let actual = lexer.next_token().unwrap();
            if actual.typ == TokenType::EndOfFile {
                break;
            }
            if actual.typ == LiteralNumber {
                assert!(
                    parse_lua_numeral(&lexer.source[actual.range()]).is_some(),
                    "lexer emitted an invalid numeral token at {}",
                    actual.start
                );
            }
            let expected = tokens.next().unwrap();
            assert_eq!(expected, actual);
        }
        assert!(tokens.next().is_none());
        assert_eq!(lines, lexer.linebreaks.as_slice());
    }

    fn check_line(input: &str, tokens: &[(TokenType, usize, u32)]) {
        check(input, tokens, &[0]);
    }

    #[test]
    fn test_lexer01() {
        let tokens = &[(LiteralNumber, 0, 2)];
        check_line("50", tokens);
    }

    #[test]
    fn test_lexer02() {
        let input = "hi 4 false";
        let tokens = &[(Identifier, 0, 2), (LiteralNumber, 3, 1), (False, 5, 5)];
        check_line(input, tokens);
    }

    #[test]
    fn test_lexer03() {
        let input = "hi5";
        let tokens = &[(Identifier, 0, 3)];
        check_line(input, tokens);
    }

    #[test]
    fn test_lexer04() {
        let input = "5 + 5";
        let tokens = &[(LiteralNumber, 0, 1), (Plus, 2, 1), (LiteralNumber, 4, 1)];
        check_line(input, tokens);
    }

    #[test]
    fn test_lexer05() {
        let input = "print 5 or 6;";
        let tokens = &[
            (Identifier, 0, 5),
            (LiteralNumber, 6, 1),
            (Or, 8, 2),
            (LiteralNumber, 11, 1),
            (Semi, 12, 1),
        ];
        check_line(input, tokens);
    }

    #[test]
    fn test_lexer06() {
        let input = "t = {x = 3}";
        let tokens = &[
            (Identifier, 0, 1),
            (Assign, 2, 1),
            (LCurly, 4, 1),
            (Identifier, 5, 1),
            (Assign, 7, 1),
            (LiteralNumber, 9, 1),
            (RCurly, 10, 1),
        ];
        check_line(input, tokens);
    }

    #[test]
    fn consecutive_comments_are_stack_safe() {
        // A long run of comments used to tail-recurse through next_token and
        // could overflow the stack. It must lex to the trailing token
        // iteratively.
        let mut src = String::new();
        for _ in 0..100_000 {
            src.push_str("-- comment\n");
        }
        src.push('x');
        let mut lexer = Lexer::new(src.as_bytes());
        assert_eq!(lexer.next_token().unwrap().typ, TokenType::Identifier);
        assert_eq!(lexer.next_token().unwrap().typ, TokenType::EndOfFile);
    }

    #[test]
    fn bare_cr_counts_as_newline() {
        // "a\rb": a bare '\r' begins a new line, so a linebreak is recorded at
        // position 2.
        check("a\rb", &[(Identifier, 0, 1), (Identifier, 2, 1)], &[0, 2]);
    }

    #[test]
    fn crlf_counts_once() {
        // "a\r\nb": '\r\n' is a single newline, counted via its '\n' at pos 3.
        check("a\r\nb", &[(Identifier, 0, 1), (Identifier, 3, 1)], &[0, 3]);
    }

    #[test]
    fn crlf_then_cr_counts_twice() {
        // Reference `inclinenumber` folds one pair per break: "\r\n" is one
        // break and the trailing '\r' a second, so `b` sits on line 3.
        check(
            "a\r\n\rb",
            &[(Identifier, 0, 1), (Identifier, 4, 1)],
            &[0, 3, 4],
        );
        check(
            "a\n\r\nb",
            &[(Identifier, 0, 1), (Identifier, 4, 1)],
            &[0, 3, 4],
        );
        check(
            "a\r\n\r\nb",
            &[(Identifier, 0, 1), (Identifier, 5, 1)],
            &[0, 3, 5],
        );
    }

    #[test]
    fn escaped_crlf_then_cr_leaves_string_unclosed() {
        // The escape swallows exactly one "\r\n" pair; the following bare
        // '\r' is an unescaped newline that ends the short string.
        let err = Lexer::new("\"a\\\r\n\rb\"".as_bytes())
            .next_token()
            .expect_err("an unescaped CR must end the short string");
        assert!(matches!(
            err.kind(),
            crate::error::ErrorKind::SyntaxError(SyntaxError::UnclosedString)
        ));
    }

    #[test]
    fn short_strings_are_decoded_by_the_lexer_and_bad_escapes_are_deferred() {
        let mut input = TokenStream::new("\"a\\x41\\n\"".as_bytes());
        let token = input.next().expect("closed string must lex");
        assert_eq!(token.typ, LiteralString);
        match input
            .take_literal_string_contents(token.start)
            .expect("lexer must retain decoded string contents")
        {
            LiteralStringContents::Decoded(contents) => {
                assert_eq!(contents.as_slice(), &b"aA\n"[..]);
            }
            _ => panic!("valid escapes must decode successfully"),
        }

        let mut input = TokenStream::new("\"\\q\"".as_bytes());
        let token = input
            .next()
            .expect("invalid escapes remain deferred until parser consumption");
        assert_eq!(token.typ, LiteralString);
        assert!(matches!(
            input.take_literal_string_contents(token.start),
            Some(LiteralStringContents::SyntaxError {
                kind: SyntaxError::InvalidEscapeSequence,
                pos: 1,
            })
        ));
    }

    #[test]
    fn bare_cr_in_short_string_is_unclosed() {
        let source = "\"a\rb\"";
        assert_eq!(source.as_bytes()[2], b'\r');
        let err = Lexer::new(source.as_bytes())
            .next_token()
            .expect_err("bare CR must terminate a short string");
        assert!(matches!(
            err.kind(),
            crate::error::ErrorKind::SyntaxError(SyntaxError::UnclosedString)
        ));
    }

    #[test]
    fn vertical_tab_is_lua_whitespace() {
        let source = "left\x0bright";
        assert_eq!(source.as_bytes()[4], 0x0b);
        check_line(source, &[(Identifier, 0, 4), (Identifier, 5, 5)]);
    }

    #[test]
    fn bare_cr_counts_as_a_line_break_before_lparen() {
        let source = "f\r(g)";
        assert_eq!(source.as_bytes()[1], b'\r');
        check(
            source,
            &[
                (Identifier, 0, 1),
                (LParen, 2, 1),
                (Identifier, 3, 1),
                (RParen, 4, 1),
            ],
            &[0, 2],
        );
    }

    #[test]
    fn leveled_long_comments_use_exact_matching_level() {
        let source = "--[[ zero ]]\nzero\n--[=[ one ]=]\none\n--[==[ two ]==]\ntwo\n--[==[ ]=] still comment ]==]\nthree";
        check(
            source,
            &[
                (Identifier, 13, 4),
                (Identifier, 32, 3),
                (Identifier, 52, 3),
                (Identifier, 86, 5),
            ],
            &[0, 13, 18, 32, 36, 52, 56, 86],
        );
    }

    #[test]
    fn short_comment_ends_at_cr() {
        let source = "-- comment\rprint";
        assert_eq!(source.as_bytes()[10], b'\r');
        check(source, &[(Identifier, 11, 5)], &[0, 11]);
    }

    #[test]
    fn numeral_identifier_adjacency_is_malformed() {
        // Lua 5.2 accepts `3or`; dellingr follows Lua 5.4's stricter rule.
        for input in ["3or", "1e5or", "3_name", "0x5rad"] {
            let err = Lexer::new(input.as_bytes())
                .next_token()
                .expect_err("glued numeral and identifier must fail");
            assert!(
                matches!(
                    err.kind(),
                    crate::error::ErrorKind::SyntaxError(SyntaxError::BadNumber)
                ),
                "{input:?}"
            );
        }
    }

    #[test]
    fn decimal_exponent_validation_stays_with_the_shared_numeral_parser() {
        for input in ["1e", "1e+"] {
            let token = Lexer::new(input.as_bytes())
                .next_token()
                .expect("decimal exponent failure is deferred to parsing");
            assert_eq!(token.typ, LiteralNumber);
            assert!(parse_lua_numeral(input.as_bytes()).is_none());
        }
    }

    #[test]
    fn hex_float_literals_lex_as_one_token() {
        // Fractions and binary-exponent forms are single numeral tokens.
        check_line("0x1.8p+0", &[(LiteralNumber, 0, 8)]);
        check_line("0x1.8P-2", &[(LiteralNumber, 0, 8)]);
        check_line("0x.8", &[(LiteralNumber, 0, 4)]);
        check_line("0x1.", &[(LiteralNumber, 0, 4)]);
        check_line("0x1p2", &[(LiteralNumber, 0, 5)]);
    }

    #[test]
    fn malformed_hex_floats_are_rejected() {
        // No mantissa digits, or a 'p' exponent without digits.
        for input in ["0x.", "0xp1", "0x1p", "0x1p+", "0x1p-", "0x1p2f"] {
            let mut lexer = Lexer::new(input.as_bytes());
            let mut result = lexer.next_token();
            while let Ok(token) = &result {
                if token.typ == TokenType::EndOfFile {
                    break;
                }
                result = lexer.next_token();
            }
            assert!(result.is_err(), "{input:?} must fail to lex");
        }
    }

    #[test]
    fn malformed_hex_number_errors_keep_their_original_columns() {
        for (input, column) in [("0x.", 4), ("0xp1", 3), ("0x1p+", 6)] {
            let err = Lexer::new(input.as_bytes())
                .next_token()
                .expect_err("malformed hexadecimal numeral must fail in the lexer");
            assert!(matches!(
                err.kind(),
                crate::error::ErrorKind::SyntaxError(SyntaxError::BadNumber)
            ));
            assert_eq!(err.column(), column, "{input:?}");
        }
    }

    #[test]
    fn test_lexer08() {
        let input = "print {x = 5,}";
        let tokens = &[
            (Identifier, 0, 5),
            (LCurly, 6, 1),
            (Identifier, 7, 1),
            (Assign, 9, 1),
            (LiteralNumber, 11, 1),
            (Comma, 12, 1),
            (RCurly, 13, 1),
        ];
        check_line(input, tokens);
    }

    #[test]
    fn test_lexer09() {
        let input = "print()\nsome_other_function(an_argument)\n";
        let tokens = &[
            (Identifier, 0, 5),
            (LParen, 5, 1),
            (RParen, 6, 1),
            (Identifier, 8, 19),
            (LParen, 27, 1),
            (Identifier, 28, 11),
            (RParen, 39, 1),
        ];
        let linebreaks = &[0, 8, 41];
        check(input, tokens, linebreaks);
    }

    #[test]
    fn test_lexer10() {
        let input = "\n\n2\n456\n";
        let tokens = &[(LiteralNumber, 2, 1), (LiteralNumber, 4, 3)];
        let linebreaks = &[0, 1, 2, 4, 8];
        check(input, tokens, linebreaks);
    }

    #[test]
    fn test_lexer11() {
        let input = "-- basic test\nprint('hi' --comment\n )\n";
        let tokens = &[
            (Identifier, 14, 5),
            (LParen, 19, 1),
            (LiteralString, 20, 4),
            (RParen, 36, 1),
        ];
        let linebreaks = &[0, 14, 35, 38];
        check(input, tokens, linebreaks);
    }

    #[test]
    fn test_lexer12() {
        let input = "print()\n(some_other_function)(an_argument)\n";
        let tokens = &[
            (Identifier, 0, 5),
            (LParen, 5, 1),
            (RParen, 6, 1),
            (LParen, 8, 1),
            (Identifier, 9, 19),
            (RParen, 28, 1),
            (LParen, 29, 1),
            (Identifier, 30, 11),
            (RParen, 41, 1),
        ];
        let linebreaks = &[0, 8, 43];
        check(input, tokens, linebreaks);
    }

    #[test]
    fn string_escape_tokens_preserve_bounds_and_following_token() {
        check_line(
            r#""\065" next"#,
            &[(LiteralString, 0, 6), (Identifier, 7, 4)],
        );
        check_line(
            r#""\x41" next"#,
            &[(LiteralString, 0, 6), (Identifier, 7, 4)],
        );
        check_line(
            r#""a\z  b" next"#,
            &[(LiteralString, 0, 8), (Identifier, 9, 4)],
        );
    }

    #[test]
    fn multiline_z_escape_stays_within_one_string_token() {
        let input = "\"a\\z\n  b\" next";
        check(
            input,
            &[(LiteralString, 0, 9), (Identifier, 10, 4)],
            &[0, 5],
        );
    }

    #[test]
    fn long_strings_return_an_error_at_the_opening_bracket() {
        for input in ["[[hello]]", "[=[x]=]", "[["] {
            let err = Lexer::new(input.as_bytes())
                .next_token()
                .expect_err("long strings must not tokenize");
            assert!(matches!(
                err.kind(),
                crate::error::ErrorKind::SyntaxError(SyntaxError::LongStringUnsupported)
            ));
            assert_eq!((err.line_num(), err.column()), (1, 1));
        }
    }

    #[test]
    fn multiline_comments_remain_non_panicking() {
        check_line("--[[comment]] next", &[(Identifier, 14, 4)]);
    }

    #[test]
    fn unfinished_long_comment_is_an_error_at_its_opener() {
        for input in [
            "x --[[ never closed\nprint(1)",
            "x --[==[ closed at the wrong level ]=]\nprint(1)",
            "x --[[",
        ] {
            let mut lexer = Lexer::new(input.as_bytes());
            assert_eq!(lexer.next_token().unwrap().typ, Identifier);
            let err = lexer
                .next_token()
                .expect_err("an unfinished long comment must not swallow the file");
            assert!(matches!(
                err.kind(),
                crate::error::ErrorKind::SyntaxError(SyntaxError::UnfinishedLongComment)
            ));
            assert!(err.is_recoverable(), "{input:?}");
            assert_eq!((err.line_num(), err.column()), (1, 3), "{input:?}");
        }
        // A short comment running to EOF stays legal.
        check_line("x -- trailing", &[(Identifier, 0, 1)]);
        check_line("x --[ not a long opener", &[(Identifier, 0, 1)]);
    }

    #[test]
    fn lparen_is_plain_whatever_precedes_it() {
        // A `(` is always an ordinary `LParen`, on the same line or a later
        // one, with or without comments between: the parser reads it as a
        // call continuation, as reference Lua does.
        for input in [
            "f(g)",
            "f (g)",
            "f\n(g)",
            "f -- note\n(g)",
            "f\n-- note\n(g)",
            "f --[[ a\nb ]] (g)",
            "f -- note\r(g)",
            "f -- note\r\n(g)",
            "f -- note\n\r(g)",
            "f --[=[ a\rb ]=] --[[ c ]] (g)",
            "f --[[ note ]] (g)",
        ] {
            let mut lexer = Lexer::new(input.as_bytes());
            assert_eq!(lexer.next_token().unwrap().typ, Identifier);
            assert_eq!(lexer.next_token().unwrap().typ, LParen, "{input:?}");
        }
        let mut tokens = TokenStream::new("f -- note\n(g)".as_bytes());
        assert_eq!(tokens.peek2().unwrap().typ, LParen);
        assert_eq!(tokens.next().unwrap().typ, Identifier);
        assert_eq!(tokens.next().unwrap().typ, LParen);
    }

    #[test]
    fn token_lines_are_stamped_before_token_bodies() {
        for (source, expected_lines) in [
            ("a\nb", vec![1, 2, 2]),
            ("a\rb", vec![1, 2, 2]),
            ("a\r\nb", vec![1, 2, 2]),
            ("a\n\rb", vec![1, 2, 2]),
            ("a\n\nb", vec![1, 3, 3]),
            ("a\n", vec![1, 2]),
            ("a\r", vec![1, 2]),
            ("a\r\n", vec![1, 2]),
            ("a\n\r", vec![1, 2]),
            ("-- short\na", vec![2, 2]),
            ("--[[ zero\n]]\na", vec![3, 3]),
            ("--[=[ one\n]=]\na", vec![3, 3]),
            ("\"a\\z\n b\" x", vec![1, 2, 2]),
            ("\"a\\\rb\" x", vec![1, 2, 2]),
            ("\"a\\\r\nb\" x", vec![1, 2, 2]),
            ("\"a\\\n\rb\" x", vec![1, 2, 2]),
        ] {
            let mut lexer = Lexer::new(source.as_bytes());
            let mut actual_lines = Vec::new();
            loop {
                let token = lexer.next_token().expect("line fixture must lex");
                actual_lines.push(token.line);
                if token.typ == TokenType::EndOfFile {
                    break;
                }
            }
            assert_eq!(actual_lines, expected_lines, "{source:?}");
        }
    }

    #[test]
    fn peek2_preserves_line_stamps_across_lookahead() {
        let mut tokens = TokenStream::new("a\nb".as_bytes());
        assert_eq!(tokens.peek().unwrap().line, 1);
        assert_eq!(tokens.peek2().unwrap().line, 2);
        assert_eq!(tokens.next().unwrap().line, 1);
        assert_eq!(tokens.next().unwrap().line, 2);
        assert_eq!(tokens.next().unwrap().line, 2);
    }

    #[test]
    fn line_and_col_keeps_newline_boundary_tie_breaks() {
        let mut lexer = Lexer::new("a\r\nb\n\rc".as_bytes());
        while lexer.next_token().expect("boundary fixture must lex").typ != TokenType::EndOfFile {}

        for (pos, expected) in [
            (0, (1, 1)),
            (1, (1, 2)),
            (2, (1, 3)),
            (3, (2, 1)),
            (4, (2, 2)),
            (5, (2, 3)),
            (6, (3, 1)),
        ] {
            assert_eq!(lexer.line_and_col(pos), expected, "byte {pos}");
        }
    }

    #[test]
    fn lexer_error_positions_stay_pinned_across_newlines() {
        let err = Lexer::new("\r\n@".as_bytes())
            .next_token()
            .expect_err("invalid character must fail");
        assert_eq!((err.line_num(), err.column()), (2, 2));

        // Pre-existing quirk, preserved: the unterminated-string error fires
        // on the newline's FIRST byte, before a CRLF/LFCR pair's line start
        // is recorded (next_char records the pair only after its second
        // byte), so the pair renders on line 1 while a bare CR/LF - recorded
        // immediately - renders on line 2. The old linear walk saw the same
        // linebreaks state and gave the same answers.
        for (source, expected) in [
            ("\"a\r\"", (2, 1)),
            ("\"a\n\"", (2, 1)),
            ("\"a\r\n\"", (1, 4)),
            ("\"a\n\r\"", (1, 4)),
        ] {
            let err = Lexer::new(source.as_bytes())
                .next_token()
                .expect_err("physical newline must leave a string unclosed");
            assert_eq!((err.line_num(), err.column()), expected, "{source:?}");
        }
    }
}
