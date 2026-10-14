use std::ops::Range;

/// A lexical unit in the source code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Token {
    pub(super) typ: TokenType,
    pub(super) start: usize,
    pub(super) len: u32,
    /// One-based source line captured when this token starts.
    pub(super) line: u32,
}

#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum TokenType {
    // Keywords
    And, Break, Do, Else, ElseIf, End, False, For, Function, If, In, Local,
    Nil, Not, Or, Repeat, Return, Then, True, Until, While,
    // Operator symbols
    Plus, Minus, Star, Slash, Mod, Caret, Hash,
    // Comparisons
    Equal, NotEqual, LessEqual, GreaterEqual, Less, Greater,
    // L/R stuff
    LParen, LParenLineStart, RParen, LCurly, RCurly, LSquare, RSquare,
    // Other symbols
    Semi, Colon, Comma, Dot, DotDot, DotDotDot, Assign,
    // Others
    Identifier,
    LiteralNumber,
    LiteralHexNumber,
    LiteralString,

    EndOfFile,
}

impl TokenType {
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

    pub(super) fn keyword_match(s: &str) -> Self {
        match Self::KEYWORDS.binary_search_by(|token_type| token_type.spelling().cmp(s)) {
            Ok(index) => Self::KEYWORDS[index],
            Err(_) => Self::Identifier,
        }
    }

    pub(super) const fn spelling(self) -> &'static str {
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
            Self::LParen | Self::LParenLineStart => "(",
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
            Self::LiteralNumber | Self::LiteralHexNumber => "<number>",
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
