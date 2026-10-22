use std::ops::Range;

/// A lexical unit in the source code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Token {
    pub(super) typ: TokenKind,
    pub(super) start: usize,
    pub(super) len: u32,
    /// One-based source line captured when this token starts.
    pub(super) line: u32,
}

// The token vocabulary is public, as `error::TokenKind`, because syntax errors
// name the token the grammar expected. Its definition lives there; the lexer
// helpers and the spelling table stay here.
pub(super) use crate::error::TokenKind;

impl TokenKind {
    // Keep this list sorted by `spelling()` for keyword binary search.
    const KEYWORDS: &[Self] = &[
        Self::And,
        Self::Break,
        Self::Do,
        Self::Else,
        Self::ElseIf,
        Self::End,
        Self::False,
        Self::For,
        Self::Function,
        Self::If,
        Self::In,
        Self::Local,
        Self::Nil,
        Self::Not,
        Self::Or,
        Self::Repeat,
        Self::Return,
        Self::Then,
        Self::True,
        Self::Until,
        Self::While,
    ];

    pub(super) fn keyword_match(word: &[u8]) -> Self {
        match Self::KEYWORDS
            .binary_search_by(|token_type| token_type.spelling().as_bytes().cmp(word))
        {
            Ok(index) => Self::KEYWORDS[index],
            Err(_) => Self::Identifier,
        }
    }

    /// The token's source spelling, or its category in angle brackets for
    /// names, literals and end of input. Unquoted: keyword lookup compares
    /// against it, and `Display` adds reference Lua's quoting.
    pub(crate) const fn spelling(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Break => "break",
            Self::Do => "do",
            Self::Else => "else",
            Self::ElseIf => "elseif",
            Self::End => "end",
            Self::False => "false",
            Self::For => "for",
            Self::Function => "function",
            Self::If => "if",
            Self::In => "in",
            Self::Local => "local",
            Self::Nil => "nil",
            Self::Not => "not",
            Self::Or => "or",
            Self::Repeat => "repeat",
            Self::Return => "return",
            Self::Then => "then",
            Self::True => "true",
            Self::Until => "until",
            Self::While => "while",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Mod => "%",
            Self::Caret => "^",
            Self::Hash => "#",
            Self::Equal => "==",
            Self::NotEqual => "~=",
            Self::LessEqual => "<=",
            Self::GreaterEqual => ">=",
            Self::Less => "<",
            Self::Greater => ">",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LCurly => "{",
            Self::RCurly => "}",
            Self::LSquare => "[",
            Self::RSquare => "]",
            Self::Semi => ";",
            Self::Colon => ":",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::DotDot => "..",
            Self::DotDotDot => "...",
            Self::Assign => "=",
            Self::Identifier => "<name>",
            Self::LiteralNumber => "<number>",
            Self::LiteralString => "<string>",
            Self::EndOfFile => "<eof>",
        }
    }
}

impl Token {
    pub(super) fn range(&self) -> Range<usize> {
        let start = self.start;
        let end = start + self.len as usize;
        start..end
    }
}
